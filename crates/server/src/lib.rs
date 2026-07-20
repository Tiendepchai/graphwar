pub mod auth;
pub mod config;
pub mod rooms;

use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        ConnectInfo, State,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
use graphwar_protocol::{ClientMessage, ServerMessage, PROTOCOL_VERSION};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::RwLock;
use uuid::Uuid;

pub use config::Config;
use rooms::{RoomError, RoomRegistry};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub rooms: RoomRegistry,
}

impl AppState {
    pub fn new(pool: PgPool, config: Config) -> Self {
        Self {
            pool,
            config: Arc::new(config),
            rooms: Arc::new(RwLock::new(Default::default())),
        }
    }
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/auth/register", post(register))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/ws", get(websocket))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

#[derive(Deserialize)]
struct Credentials {
    username: String,
    password: String,
}
#[derive(Serialize)]
struct UserResponse {
    id: Uuid,
    username: String,
}

async fn register(
    State(state): State<AppState>,
    Json(input): Json<Credentials>,
) -> Result<(StatusCode, Json<UserResponse>), ApiError> {
    let user = auth::register(&state.pool, input.username, input.password).await?;
    Ok((
        StatusCode::CREATED,
        Json(UserResponse {
            id: user.id,
            username: user.username,
        }),
    ))
}

async fn login(
    State(state): State<AppState>,
    Json(input): Json<Credentials>,
) -> Result<(HeaderMap, Json<UserResponse>), ApiError> {
    let user = auth::authenticate(&state.pool, &input.username, &input.password).await?;
    let token = auth::create_session(&state.pool, user.id, state.config.session_ttl).await?;
    let cookie = auth::session_cookie(&token, state.config.secure_cookies)?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| ApiError::internal())?,
    );
    Ok((
        headers,
        Json(UserResponse {
            id: user.id,
            username: user.username,
        }),
    ))
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<HeaderMap, ApiError> {
    if let Some(token) = auth::session_from_headers(&headers) {
        auth::delete_session(&state.pool, &token).await?;
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        HeaderValue::from_static("graphwar_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"),
    );
    Ok(headers)
}

async fn websocket(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
) -> Result<Response, ApiError> {
    verify_origin(&headers, &state.config)?;
    let user = auth::user_from_headers(&state.pool, &headers)
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    Ok(ws
        .on_upgrade(move |socket| handle_socket(socket, state, user.id, peer))
        .into_response())
}

async fn handle_socket(
    mut socket: WebSocket,
    state: AppState,
    user_id: Uuid,
    _peer: std::net::SocketAddr,
) {
    let _ = socket
        .send(Message::Text(
            serde_json::to_string(&ServerMessage::Hello {
                version: PROTOCOL_VERSION,
            })
            .unwrap()
            .into(),
        ))
        .await;
    while let Some(Ok(Message::Text(text))) = socket.next().await {
        let response = match serde_json::from_str::<ClientMessage>(&text) {
            Ok(message) => dispatch(&state, user_id, message).await,
            Err(_) => Err(RoomError::Invalid("invalid JSON message")),
        };
        let message = match response {
            Ok(message) => message,
            Err(error) => ServerMessage::Error {
                code: error.code().into(),
                message: error.to_string(),
            },
        };
        if socket
            .send(Message::Text(
                serde_json::to_string(&message).unwrap().into(),
            ))
            .await
            .is_err()
        {
            break;
        }
    }
}

async fn dispatch(
    state: &AppState,
    user_id: Uuid,
    message: ClientMessage,
) -> Result<ServerMessage, RoomError> {
    let mut rooms = state.rooms.write().await;
    match message {
        ClientMessage::Hello { version } if version == PROTOCOL_VERSION => {
            Ok(ServerMessage::Hello { version })
        }
        ClientMessage::Hello { .. } => Err(RoomError::Invalid("unsupported protocol version")),
        ClientMessage::CreateRoom { name, visibility } => Ok(ServerMessage::Room {
            snapshot: rooms.create(user_id, name, visibility)?,
        }),
        ClientMessage::JoinRoom { room_id, invite } => Ok(ServerMessage::Room {
            snapshot: rooms.join(user_id, room_id, invite.as_deref())?,
        }),
        ClientMessage::LeaveRoom => {
            rooms.leave(user_id)?;
            Ok(ServerMessage::RoomList {
                rooms: rooms.public_snapshots(),
            })
        }
        ClientMessage::SetReady { ready } => Ok(ServerMessage::Room {
            snapshot: rooms.set_ready(user_id, ready)?,
        }),
        ClientMessage::StartGame => Ok(ServerMessage::Room {
            snapshot: rooms.start_game(user_id)?,
        }),
        ClientMessage::Chat { text } if text.trim().is_empty() || text.len() > 500 => {
            Err(RoomError::Invalid("chat must be 1-500 characters"))
        }
        ClientMessage::Chat { text } => {
            rooms.require_member(user_id)?;
            Ok(ServerMessage::Chat {
                player_id: user_id,
                text,
            })
        }
    }
}

fn verify_origin(headers: &HeaderMap, config: &Config) -> Result<(), ApiError> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(ApiError::forbidden)?;
    if config
        .allowed_origins
        .iter()
        .any(|allowed| allowed == origin)
    {
        Ok(())
    } else {
        Err(ApiError::forbidden())
    }
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
    fn internal() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "internal error",
        }
    }
}
impl From<auth::AuthError> for ApiError {
    fn from(_: auth::AuthError) -> Self {
        Self::unauthorized()
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
}
