pub mod auth;
pub mod bot;
pub mod config;
pub mod rooms;

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::extract::DefaultBodyLimit;
use axum::{
    Json, Router,
    extract::{
        ConnectInfo, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{Sink, SinkExt, StreamExt};
use graphwar_protocol::{
    AccountResponse, ClientMessage, LoginRequest, PROTOCOL_VERSION, RegisterRequest, ServerMessage,
};
use sqlx::PgPool;
use tokio::sync::{RwLock, broadcast};
use tower_http::services::ServeDir;

pub use config::Config;
use rooms::{RoomError, RoomRegistry};

const MAX_WS_MESSAGE_BYTES: usize = 8 * 1024;
const MAX_WS_TRANSPORT_BYTES: usize = 16 * 1024;
const MAX_HTTP_BODY_BYTES: usize = 16 * 1024;
const MAX_MESSAGES_PER_WINDOW: usize = 120;
const RATE_WINDOW: Duration = Duration::from_secs(60);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(45);
const WS_SEND_TIMEOUT: Duration = Duration::from_secs(5);
const BOT_SEARCH_BUDGET: Duration = Duration::from_secs(2);
const DISCONNECT_GRACE: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub rooms: RoomRegistry,
    events: broadcast::Sender<ScopedEvent>,
    presence: Arc<RwLock<HashMap<uuid::Uuid, usize>>>,
}

impl AppState {
    pub fn new(pool: PgPool, config: Config) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            pool,
            config: Arc::new(config),
            rooms: Arc::new(RwLock::new(Default::default())),
            events,
            presence: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn expire_turns(&self) {
        let outcomes = self.rooms.write().await.expire_turns();
        for outcome in outcomes {
            self.broadcast_turn(outcome);
        }
        self.drive_bots().await;
        self.cleanup_inactive_connections().await;
    }

    async fn drive_bots(&self) {
        let turns = self.rooms.read().await.pending_bot_turns();
        for turn in turns {
            let mode = turn.mode;
            let terrain = turn.terrain.clone();
            let state = turn.state.clone();
            let team = turn.team;
            let level = turn.level;
            let seed = turn.seed;
            let memory = turn.memory.clone();
            let result = tokio::task::spawn_blocking(move || {
                crate::bot::search(crate::bot::SearchInput {
                    mode,
                    terrain: &terrain,
                    state: &state,
                    team,
                    level,
                    seed,
                    memory,
                    budget: BOT_SEARCH_BUDGET,
                })
            })
            .await
            .ok();
            let mut rooms = self.rooms.write().await;
            if let Some(outcome) =
                result.and_then(|result| rooms.apply_bot_turn(turn.clone(), result).ok().flatten())
            {
                drop(rooms);
                self.broadcast_fire(outcome);
            } else if let Some(outcome) = rooms.skip_bot_turn(turn).ok().flatten() {
                drop(rooms);
                self.broadcast_turn(outcome);
            }
        }
    }

    fn broadcast_turn(&self, outcome: rooms::StartOutcome) {
        let players = outcome
            .snapshot
            .players
            .iter()
            .filter(|player| !player.is_bot)
            .map(|player| player.id)
            .collect();
        let _ = self.events.send(ScopedEvent {
            audience: Audience::Room {
                room_id: outcome.snapshot.id,
                players,
            },
            message: ServerMessage::TurnStarted {
                snapshot: outcome.snapshot,
                game: outcome.game,
            },
        });
    }

    fn broadcast_fire(&self, outcome: rooms::FireOutcome) {
        let players = outcome
            .snapshot
            .players
            .iter()
            .filter(|player| !player.is_bot)
            .map(|player| player.id)
            .collect();
        let message = if outcome.shot.winner_team.is_some() {
            ServerMessage::GameFinished {
                snapshot: outcome.snapshot,
                shot: outcome.shot,
            }
        } else {
            ServerMessage::ShotResolved {
                snapshot: outcome.snapshot,
                shot: outcome.shot,
            }
        };
        let room_id = match &message {
            ServerMessage::GameFinished { snapshot, .. }
            | ServerMessage::ShotResolved { snapshot, .. } => snapshot.id,
            _ => unreachable!(),
        };
        let _ = self.events.send(ScopedEvent {
            audience: Audience::Room { room_id, players },
            message,
        });
    }

    async fn socket_connected(&self, player: uuid::Uuid) {
        self.presence
            .write()
            .await
            .entry(player)
            .and_modify(|count| *count += 1)
            .or_insert(1);
    }

    fn socket_disconnected(&self, player: uuid::Uuid) {
        let state = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(DISCONNECT_GRACE).await;
            if state.release_socket(player).await {
                state.cleanup_disconnected(player).await;
            }
        });
    }

    async fn release_socket(&self, player: uuid::Uuid) -> bool {
        let mut presence = self.presence.write().await;
        let Some(count) = presence.get_mut(&player) else {
            return false;
        };
        *count = count.saturating_sub(1);
        *count == 0
    }

    async fn cleanup_disconnected(&self, player: uuid::Uuid) {
        let cleanup = {
            let presence = self.presence.write().await;
            if presence.get(&player).is_some_and(|count| *count > 0) {
                return;
            }
            let mut rooms = self.rooms.write().await;
            let old_room = rooms
                .member_snapshot(player)
                .map(|snapshot| snapshot.id)
                .ok();
            let cleanup = match old_room {
                Some(old_room) => match rooms.leave(player) {
                    Ok(snapshot) => Some((
                        old_room,
                        snapshot,
                        rooms.member_ids(old_room),
                        rooms.public_snapshots(),
                    )),
                    Err(RoomError::WrongPhase) => None,
                    Err(_) => None,
                },
                None => Some((
                    uuid::Uuid::nil(),
                    None,
                    Vec::new(),
                    rooms.public_snapshots(),
                )),
            };
            drop(rooms);
            drop(presence);
            cleanup
        };
        let Some((old_room, snapshot, players, public_rooms)) = cleanup else {
            return;
        };
        if let Some(snapshot) = snapshot {
            let _ = self.events.send(ScopedEvent {
                audience: Audience::Room {
                    room_id: old_room,
                    players,
                },
                message: ServerMessage::Room { snapshot },
            });
        }
        let _ = self.events.send(ScopedEvent {
            audience: Audience::Lobby,
            message: ServerMessage::RoomList {
                rooms: public_rooms,
            },
        });
        let mut presence = self.presence.write().await;
        if presence.get(&player) == Some(&0) {
            presence.remove(&player);
        }
    }

    async fn cleanup_inactive_connections(&self) {
        let players = self
            .presence
            .read()
            .await
            .iter()
            .filter_map(|(player, count)| (*count == 0).then_some(*player))
            .collect::<Vec<_>>();
        for player in players {
            self.cleanup_disconnected(player).await;
        }
    }
}

pub fn app(state: AppState) -> Router {
    let static_dir = state.config.static_dir.clone();
    Router::new()
        .route("/healthz", get(healthz))
        .route("/auth/register", post(register))
        .route("/auth/login", post(login))
        .route("/auth/me", get(current_user))
        .route("/auth/logout", post(logout))
        .route("/ws", get(websocket))
        .fallback_service(ServeDir::new(static_dir).append_index_html_on_directories(true))
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
        .with_state(state)
}

async fn healthz(State(state): State<AppState>) -> Result<&'static str, StatusCode> {
    sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
        .map(|_| "ok")
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
}

fn account_response(user: &auth::User) -> AccountResponse {
    AccountResponse {
        id: user.id,
        email: user.email.clone(),
        display_name: user.display_name.clone(),
    }
}

async fn register(
    State(state): State<AppState>,
    Json(input): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<AccountResponse>), ApiError> {
    let user = auth::register(&state.pool, input.email, input.display_name, input.password).await?;
    Ok((StatusCode::CREATED, Json(account_response(&user))))
}

async fn login(
    State(state): State<AppState>,
    Json(input): Json<LoginRequest>,
) -> Result<(HeaderMap, Json<AccountResponse>), ApiError> {
    let user = auth::authenticate(&state.pool, &input.email, &input.password).await?;
    let token = auth::create_session(&state.pool, user.id, state.config.session_ttl).await?;
    let cookie = auth::session_cookie(&token, state.config.secure_cookies);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| ApiError::internal())?,
    );
    Ok((headers, Json(account_response(&user))))
}

async fn current_user(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<AccountResponse>, ApiError> {
    let user = auth::user_from_headers(&state.pool, &headers)
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    Ok(Json(account_response(&user)))
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<StatusCode, ApiError> {
    if let Some(token) = auth::session_from_headers(&headers) {
        auth::delete_session(&state.pool, &token).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn websocket(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Result<Response, ApiError> {
    verify_origin(&headers, &state.config)?;
    let token = auth::session_from_headers(&headers).ok_or_else(ApiError::unauthorized)?;
    let user = auth::user_from_headers(&state.pool, &headers)
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    Ok(ws
        .max_message_size(MAX_WS_TRANSPORT_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, state, user, token, peer))
        .into_response())
}

async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    user: auth::User,
    session_token: String,
    _peer: SocketAddr,
) {
    state.socket_connected(user.id).await;
    let (mut sender, mut receiver) = socket.split();
    let mut events = state.events.subscribe();
    if send_message(
        &mut sender,
        &ServerMessage::Hello {
            version: PROTOCOL_VERSION,
        },
    )
    .await
    .is_err()
    {
        state.socket_disconnected(user.id);
        return;
    }

    let broadcast_sender = state.events.clone();
    let mut hello_complete = false;
    let mut window_started = Instant::now();
    let mut message_count = 0;
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_seen = Instant::now();
    loop {
        tokio::select! {
            incoming = receiver.next() => {
                let Some(message) = incoming else {
                    break;
                };
                let message = match message {
                    Ok(message) => message,
                    Err(error) => {
                        if error.to_string().contains("Message too long:") {
                            let _ = send_frame(
                                &mut sender,
                                Message::Close(Some(axum::extract::ws::CloseFrame {
                                    code: 1009,
                                    reason: "message too large".into(),
                                })),
                            )
                            .await;
                        }
                        break;
                    }
                };
                let message_len = match &message {
                    Message::Text(text) => text.len(),
                    Message::Binary(bytes) => bytes.len(),
                    _ => 0,
                };
                if message_len > MAX_WS_MESSAGE_BYTES {
                    let _ = send_frame(
                        &mut sender,
                        Message::Close(Some(axum::extract::ws::CloseFrame {
                            code: 1009,
                            reason: "message too large".into(),
                        })),
                    )
                    .await;
                    break;
                }
                match auth::session_is_valid(&state.pool, user.id, &session_token).await {
                    Ok(true) => {}
                    Ok(false) => {
                        let _ = send_message(&mut sender, &ServerMessage::SessionExpired).await;
                        break;
                    }
                    Err(_) => break,
                }
                last_seen = Instant::now();
                if window_started.elapsed() >= RATE_WINDOW {
                    window_started = Instant::now();
                    message_count = 0;
                }
                message_count += 1;
                let Message::Text(text) = message else {
                    if message_count > MAX_MESSAGES_PER_WINDOW {
                        break;
                    }
                    continue;
                };
                if message_count > MAX_MESSAGES_PER_WINDOW {
                    let message = ServerMessage::Error {
                        code: "rate_limited".into(),
                        message: "too many messages; retry shortly".into(),
                    };
                    let _ = send_message(&mut sender, &message).await;
                    break;
                }
                let response = match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(ClientMessage::Hello { version }) if version == PROTOCOL_VERSION => {
                        hello_complete = true;
                        Ok(DispatchOutcome::private(ServerMessage::Hello { version }))
                    }
                    Ok(ClientMessage::Hello { .. }) => {
                        Err(RoomError::Invalid("unsupported protocol version"))
                    }
                    Ok(_) if !hello_complete => Err(RoomError::Invalid("hello is required first")),
                    Ok(message) => dispatch(&state, &user, message).await,
                    Err(_) => Err(RoomError::Invalid("invalid JSON message")),
                };
                match response {
                    Ok(outcome) => {
                        if let Some(message) = outcome.private {
                            if send_message(&mut sender, &message).await.is_err() {
                                break;
                            }
                            if matches!(message, ServerMessage::Hello { .. })
                                && send_sync(&state, user.id, &mut sender).await.is_err()
                            {
                                break;
                            }
                        }
                        for event in outcome.broadcasts {
                            if broadcast_sender.send(event).is_err() {
                                break;
                            }
                        }
                    }
                    Err(error) => {
                        let message = ServerMessage::Error {
                            code: error.code().into(),
                            message: error.to_string(),
                        };
                        if send_message(&mut sender, &message).await.is_err() {
                            break;
                        }
                    }
                }
            }
            event = events.recv() => {
                match auth::session_is_valid(&state.pool, user.id, &session_token).await {
                    Ok(true) => {}
                    Ok(false) => {
                        let _ = send_message(&mut sender, &ServerMessage::SessionExpired).await;
                        break;
                    }
                    Err(_) => break,
                }
                if !hello_complete {
                    continue;
                }
                match event {
                    Ok(event) if event_visible_to(&state, user.id, &event).await => {
                        if send_message(&mut sender, &event.message).await.is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        if send_sync(&state, user.id, &mut sender).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = heartbeat.tick() => {
                match auth::session_is_valid(&state.pool, user.id, &session_token).await {
                    Ok(true) => {}
                    Ok(false) => {
                        let _ = send_message(&mut sender, &ServerMessage::SessionExpired).await;
                        break;
                    }
                    Err(_) => break,
                }
                if last_seen.elapsed() >= HEARTBEAT_TIMEOUT
                    || send_frame(&mut sender, Message::Ping(Vec::new().into())).await.is_err()
                {
                    break;
                }
            }
        }
    }
    state.socket_disconnected(user.id);
}

async fn send_message<S, E>(socket: &mut S, message: &ServerMessage) -> Result<(), ()>
where
    S: Sink<Message, Error = E> + Unpin,
{
    send_frame(
        socket,
        Message::Text(serde_json::to_string(message).map_err(|_| ())?.into()),
    )
    .await
}

async fn send_frame<S, E>(socket: &mut S, message: Message) -> Result<(), ()>
where
    S: Sink<Message, Error = E> + Unpin,
{
    tokio::time::timeout(WS_SEND_TIMEOUT, socket.send(message))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
}

struct DispatchOutcome {
    private: Option<ServerMessage>,
    broadcasts: Vec<ScopedEvent>,
}

#[derive(Clone)]
struct ScopedEvent {
    audience: Audience,
    message: ServerMessage,
}

#[derive(Clone)]
enum Audience {
    Lobby,
    Accounts(Vec<uuid::Uuid>),
    Room {
        room_id: uuid::Uuid,
        players: Vec<uuid::Uuid>,
    },
}

impl DispatchOutcome {
    fn private(message: ServerMessage) -> Self {
        Self {
            private: Some(message),
            broadcasts: Vec::new(),
        }
    }

    fn accounts(accounts: Vec<uuid::Uuid>, message: ServerMessage) -> Self {
        Self {
            private: None,
            broadcasts: vec![ScopedEvent {
                audience: Audience::Accounts(accounts),
                message,
            }],
        }
    }

    fn room(room_id: uuid::Uuid, players: Vec<uuid::Uuid>, message: ServerMessage) -> Self {
        Self {
            private: None,
            broadcasts: vec![ScopedEvent {
                audience: Audience::Room { room_id, players },
                message,
            }],
        }
    }

    fn with_lobby(mut self, rooms: Vec<graphwar_protocol::RoomSnapshot>) -> Self {
        self.broadcasts.push(ScopedEvent {
            audience: Audience::Lobby,
            message: ServerMessage::RoomList { rooms },
        });
        self
    }
}

async fn dispatch(
    state: &AppState,
    user: &auth::User,
    message: ClientMessage,
) -> Result<DispatchOutcome, RoomError> {
    let mut rooms = state.rooms.write().await;
    match message {
        ClientMessage::Hello { .. } => Err(RoomError::Invalid("hello already completed")),
        ClientMessage::ListRooms => Ok(DispatchOutcome::private(ServerMessage::RoomList {
            rooms: rooms.public_snapshots(),
        })),
        ClientMessage::CreateRoom { name, visibility } => {
            let (snapshot, invite) =
                rooms.create(user.id, user.display_name.clone(), name, visibility)?;
            Ok(DispatchOutcome::accounts(
                vec![user.id],
                ServerMessage::RoomCreated { snapshot, invite },
            )
            .with_lobby(rooms.public_snapshots()))
        }
        ClientMessage::JoinRoom { room_id, invite } => {
            let snapshot = rooms.join(
                user.id,
                user.display_name.clone(),
                room_id,
                invite.as_deref(),
            )?;
            Ok(DispatchOutcome::room(
                snapshot.id,
                rooms.member_ids(snapshot.id),
                ServerMessage::Room { snapshot },
            )
            .with_lobby(rooms.public_snapshots()))
        }
        ClientMessage::LeaveRoom => {
            let old_room = rooms.member_snapshot(user.id)?.id;
            let snapshot = rooms.leave(user.id)?;
            let mut outcome = DispatchOutcome::accounts(vec![user.id], ServerMessage::LeftRoom);
            if let Some(snapshot) = snapshot {
                outcome.broadcasts.push(ScopedEvent {
                    audience: Audience::Room {
                        room_id: old_room,
                        players: rooms.member_ids(old_room),
                    },
                    message: ServerMessage::Room { snapshot },
                });
            }
            Ok(outcome.with_lobby(rooms.public_snapshots()))
        }
        ClientMessage::SetReady { ready } => {
            let snapshot = rooms.set_ready(user.id, ready)?;
            Ok(DispatchOutcome::room(
                snapshot.id,
                rooms.member_ids(snapshot.id),
                ServerMessage::Room { snapshot },
            ))
        }
        ClientMessage::SetMode { mode } => {
            let snapshot = rooms.set_mode(user.id, mode)?;
            Ok(DispatchOutcome::room(
                snapshot.id,
                rooms.member_ids(snapshot.id),
                ServerMessage::Room { snapshot },
            ))
        }
        ClientMessage::SetTeam { player_id, team } => {
            let snapshot = rooms.set_team(user.id, player_id, team)?;
            Ok(DispatchOutcome::room(
                snapshot.id,
                rooms.member_ids(snapshot.id),
                ServerMessage::Room { snapshot },
            ))
        }
        ClientMessage::SetSoldiers {
            player_id,
            soldiers,
        } => {
            let snapshot = rooms.set_soldiers(user.id, player_id, soldiers)?;
            Ok(DispatchOutcome::room(
                snapshot.id,
                rooms.member_ids(snapshot.id),
                ServerMessage::Room { snapshot },
            ))
        }
        ClientMessage::AddBot { level } => {
            let snapshot = rooms.add_bot(user.id, level)?;
            Ok(DispatchOutcome::room(
                snapshot.id,
                rooms.member_ids(snapshot.id),
                ServerMessage::Room { snapshot },
            )
            .with_lobby(rooms.public_snapshots()))
        }
        ClientMessage::RemoveBot { player_id } => {
            let snapshot = rooms.remove_bot(user.id, player_id)?;
            Ok(DispatchOutcome::room(
                snapshot.id,
                rooms.member_ids(snapshot.id),
                ServerMessage::Room { snapshot },
            )
            .with_lobby(rooms.public_snapshots()))
        }
        ClientMessage::StartGame => {
            let start = rooms.start_game(user.id)?;
            Ok(DispatchOutcome::room(
                start.snapshot.id,
                rooms.member_ids(start.snapshot.id),
                ServerMessage::GameStarted {
                    snapshot: start.snapshot,
                    game: start.game,
                },
            )
            .with_lobby(rooms.public_snapshots()))
        }
        ClientMessage::FireFunction {
            function,
            angle_deg,
        } => {
            let outcome = rooms.fire(user.id, function, angle_deg)?;
            Ok(DispatchOutcome::room(
                outcome.shot.game.room_id,
                rooms.member_ids(outcome.shot.game.room_id),
                if outcome.shot.winner_team.is_some() {
                    ServerMessage::GameFinished {
                        snapshot: outcome.snapshot,
                        shot: outcome.shot,
                    }
                } else {
                    ServerMessage::ShotResolved {
                        snapshot: outcome.snapshot,
                        shot: outcome.shot,
                    }
                },
            ))
        }
        ClientMessage::Chat { text } if text.trim().is_empty() || text.len() > 500 => {
            Err(RoomError::Invalid("chat must be 1-500 characters"))
        }
        ClientMessage::Chat { text } => {
            let room_id = rooms.member_snapshot(user.id)?.id;
            Ok(DispatchOutcome::room(
                room_id,
                rooms.member_ids(room_id),
                ServerMessage::Chat {
                    player_id: user.id,
                    text,
                },
            ))
        }
    }
}

async fn send_sync<S, E>(state: &AppState, player: uuid::Uuid, sender: &mut S) -> Result<(), ()>
where
    S: Sink<Message, Error = E> + Unpin,
{
    let messages = {
        let rooms = state.rooms.read().await;
        let room = match rooms.member_state(player) {
            Ok((snapshot, game)) => ServerMessage::StateSync { snapshot, game },
            Err(RoomError::NotMember) => ServerMessage::LeftRoom,
            Err(_) => return Err(()),
        };
        (
            room,
            ServerMessage::RoomList {
                rooms: rooms.public_snapshots(),
            },
        )
    };
    send_message(sender, &messages.0).await?;
    send_message(sender, &messages.1).await
}

async fn event_visible_to(state: &AppState, player: uuid::Uuid, event: &ScopedEvent) -> bool {
    match &event.audience {
        Audience::Lobby => true,
        Audience::Accounts(accounts) => accounts.contains(&player),
        Audience::Room { room_id, players } => {
            players.contains(&player) && state.rooms.read().await.is_member_of(player, *room_id)
        }
    }
}

fn verify_origin(headers: &HeaderMap, config: &Config) -> Result<(), ApiError> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(ApiError::forbidden)?;
    config
        .allowed_origins
        .iter()
        .any(|allowed| allowed == origin)
        .then_some(())
        .ok_or_else(ApiError::forbidden)
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: &'static str,
}

impl ApiError {
    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "authentication required",
        }
    }

    fn forbidden() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: "origin rejected",
        }
    }

    fn bad_request(message: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message,
        }
    }

    fn conflict(message: &'static str) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message,
        }
    }

    fn internal() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "internal error",
        }
    }
}

impl From<auth::AuthError> for ApiError {
    fn from(error: auth::AuthError) -> Self {
        match error {
            auth::AuthError::InvalidCredentials => Self::unauthorized(),
            auth::AuthError::InvalidInput(message) => Self::bad_request(message),
            auth::AuthError::EmailTaken => Self::conflict("email already registered"),
            auth::AuthError::Database(_) | auth::AuthError::PasswordHash => Self::internal(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({"error": self.message})),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};
    use sqlx::postgres::PgPoolOptions;
    use tower::ServiceExt;

    use super::*;

    #[test]
    fn accepts_only_configured_websocket_origins() {
        let config = Config::test();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:3000"),
        );
        assert!(verify_origin(&headers, &config).is_ok());
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.invalid"),
        );
        assert_eq!(
            verify_origin(&headers, &config).unwrap_err().status,
            StatusCode::FORBIDDEN
        );
    }

    fn test_state() -> AppState {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://unused")
            .expect("test pool");
        AppState::new(pool, Config::test())
    }

    fn test_user(id: uuid::Uuid, display_name: &str) -> auth::User {
        auth::User::test(id, display_name)
    }

    #[tokio::test]
    async fn final_lobby_disconnect_removes_member_and_promotes_owner() {
        let state = test_state();
        let owner = uuid::Uuid::new_v4();
        let guest = uuid::Uuid::new_v4();
        {
            let mut rooms = state.rooms.write().await;
            let room = rooms
                .create(
                    owner,
                    "Owner".into(),
                    "room".into(),
                    graphwar_protocol::RoomVisibility::Public,
                )
                .unwrap()
                .0;
            rooms.join(guest, "Guest".into(), room.id, None).unwrap();
        }
        state.socket_connected(owner).await;
        assert!(state.release_socket(owner).await);
        state.cleanup_disconnected(owner).await;

        let snapshot = state.rooms.read().await.member_snapshot(guest).unwrap();
        assert_eq!(snapshot.players.len(), 1);
        assert_eq!(snapshot.players[0].id, guest);
        assert!(snapshot.players[0].owner);
    }

    #[tokio::test]
    async fn remaining_or_reconnected_socket_prevents_cleanup() {
        let state = test_state();
        let player = uuid::Uuid::new_v4();
        state
            .rooms
            .write()
            .await
            .create(
                player,
                "Player".into(),
                "room".into(),
                graphwar_protocol::RoomVisibility::Public,
            )
            .unwrap();
        state.socket_connected(player).await;
        state.socket_connected(player).await;
        assert!(!state.release_socket(player).await);
        state.cleanup_disconnected(player).await;
        assert!(state.rooms.read().await.member_snapshot(player).is_ok());

        assert!(state.release_socket(player).await);
        state.socket_connected(player).await;
        state.cleanup_disconnected(player).await;
        assert!(state.rooms.read().await.member_snapshot(player).is_ok());
    }

    #[tokio::test]
    async fn private_room_creation_targets_every_account_socket() {
        let state = test_state();
        let player = uuid::Uuid::new_v4();
        let user = test_user(player, "Player");
        let outcome = dispatch(
            &state,
            &user,
            ClientMessage::CreateRoom {
                name: "private".into(),
                visibility: graphwar_protocol::RoomVisibility::Private,
            },
        )
        .await
        .unwrap();
        assert!(outcome.private.is_none());
        assert!(outcome.broadcasts.iter().any(|event| {
            matches!(&event.audience, Audience::Accounts(accounts) if accounts == &[player])
                && matches!(
                    event.message,
                    ServerMessage::RoomCreated {
                        invite: Some(_),
                        ..
                    }
                )
        }));
    }

    #[tokio::test]
    async fn logout_without_cookie_has_no_set_cookie_header() {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://unused")
            .expect("test pool");
        let app = app(AppState::new(pool, Config::test()));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/logout")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(response.headers().get(header::SET_COOKIE).is_none());
    }
}
