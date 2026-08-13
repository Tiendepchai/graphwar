use std::{cell::RefCell, rc::Rc};

use gloo_net::http::Request;
use gloo_timers::callback::{Interval, Timeout};
use graphwar_game_core::constants::{
    MAX_PRACTICE_TERRAIN_CIRCLES, PRACTICE_TERRAIN_RADII, SOLDIER_RADIUS,
};
use graphwar_protocol::{
    AccountResponse, ClientMessage, GameMode, LoginRequest, PROTOCOL_VERSION, Phase, PracticeSetup,
    RegisterRequest, RoomKind, RoomVisibility, ServerMessage, SetupPoint, ShotMissReason,
    ShotOutcome, TerrainCircle,
};
use uuid::Uuid;
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::spawn_local;
use web_sys::{
    AbortController, CanvasRenderingContext2d, CloseEvent, Document, DragEvent, ErrorEvent, Event,
    HtmlCanvasElement, HtmlDialogElement, HtmlFormElement, HtmlImageElement, HtmlInputElement,
    HtmlSelectElement, MessageEvent, PointerEvent, RequestCredentials, WebSocket, Window,
};

const PRESERVED_INPUTS: &[&str] = &[
    "login-email",
    "login-password",
    "register-name",
    "register-email",
    "register-password",
    "room-name",
    "room-password",
    "practice-x",
    "practice-y",
    "practice-terrain-x",
    "practice-terrain-y",
    "function-input",
    "chat-input",
];
const PRESERVED_SELECTS: &[&str] = &[
    "room-visibility",
    "room-kind",
    "practice-radius",
    "practice-soldier",
    "practice-terrain-radius",
];

#[derive(Default)]
struct FormState {
    inputs: Vec<(String, String)>,
    selects: Vec<(String, String)>,
    focus: Option<(String, Option<u32>, Option<u32>, Option<String>)>,
}

use crate::{
    animation::visible_points,
    geometry::{LOGICAL_HEIGHT, LOGICAL_WIDTH, Viewport},
    preview::trace_preview,
    state::{Action, Connection, Model, Screen, apply_pending_game, reduce},
};

struct App {
    window: Window,
    document: Document,
    model: Model,
    selected_team_player: Option<TeamMoveSelection>,
    practice_tool: PracticeTool,
    practice_radius: f64,
    practice_draft: Option<PracticeSetup>,
    practice_drag: Option<PracticeDrag>,
    practice_save: Option<PracticeSave>,
    socket: Option<WebSocket>,
    socket_handlers: Option<SocketHandlers>,
    connection_epoch: u64,
    auth_epoch: u64,
    auth_pending: bool,
    auth_request: Option<AbortController>,
    event_handlers: Vec<EventHandler>,
    dynamic_event_handlers: Vec<EventHandler>,
    reconnect_attempt: u32,
    reconnect_timer: Option<Timeout>,
    clock: Option<Interval>,
    viewport_handler: Option<Closure<dyn FnMut(Event)>>,
    expired_deadline: Option<i64>,
    shot_animation: Option<ShotAnimation>,
    soldier_sprite: Option<HtmlImageElement>,
    soldier_sprite_loaded: bool,
    soldier_helmet_team_one: Option<HtmlImageElement>,
    soldier_helmet_team_one_loaded: bool,
    soldier_helmet_team_two: Option<HtmlImageElement>,
    soldier_helmet_team_two_loaded: bool,
    soldier_sprite_handlers: Vec<SoldierSpriteHandlers>,
    ws_url: String,
}

#[derive(Clone, Copy)]
enum SoldierSpriteKind {
    Body,
    HelmetTeamOne,
    HelmetTeamTwo,
}

struct SoldierSpriteHandlers {
    _onload: Closure<dyn FnMut(Event)>,
    _onerror: Closure<dyn FnMut(Event)>,
}

struct SocketHandlers {
    _onopen: Closure<dyn FnMut(Event)>,
    _onmessage: Closure<dyn FnMut(MessageEvent)>,
    _onclose: Closure<dyn FnMut(CloseEvent)>,
    _onerror: Closure<dyn FnMut(ErrorEvent)>,
}

struct EventHandler {
    target: web_sys::EventTarget,
    event_name: &'static str,
    closure: Closure<dyn FnMut(Event)>,
}

impl Drop for EventHandler {
    fn drop(&mut self) {
        let _ = self.target.remove_event_listener_with_callback(
            self.event_name,
            self.closure.as_ref().unchecked_ref(),
        );
    }
}

struct ShotAnimation {
    sequence: u64,
    started_at: f64,
}

#[derive(Clone, Copy)]
struct TeamMoveSelection {
    player_id: Uuid,
    team: u8,
}

#[derive(Clone, Copy, Default, PartialEq)]
enum PracticeTool {
    #[default]
    Move,
    Add,
    Erase,
}

struct PracticeDrag {
    pointer_id: i32,
    player_id: Uuid,
    soldier_index: usize,
}

struct PracticeSave {
    room_id: String,
    base_revision: u64,
    submitted: PracticeSetup,
}

#[derive(Clone, Copy)]
struct CanvasPalette {
    background: &'static str,
    grid: &'static str,
    axis: &'static str,
    terrain: &'static str,
    terrain_stroke: &'static str,
    path: &'static str,
    preview: &'static str,
    team_one: &'static str,
    team_two: &'static str,
    dead: &'static str,
    soldier_stroke: &'static str,
    hit: &'static str,
}

#[derive(Clone, Copy)]
enum RenderScope {
    None,
    Header,
    Notices,
    Rooms,
    Chat,
    Screen,
}

type SharedApp = Rc<RefCell<App>>;

enum FeedEntry<'a> {
    Chat(&'a crate::state::ChatView),
    Shot(&'a crate::state::ShotHistoryView),
}

impl FeedEntry<'_> {
    fn sequence(&self) -> u64 {
        match self {
            Self::Chat(entry) => entry.sequence,
            Self::Shot(entry) => entry.sequence,
        }
    }
}

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    let window = web_sys::window().ok_or("window unavailable")?;
    let document = window.document().ok_or("document unavailable")?;
    let ws_url = websocket_url(&window)?;
    let app = Rc::new(RefCell::new(App {
        window,
        document,
        model: Model::default(),
        selected_team_player: None,
        practice_tool: PracticeTool::Move,
        practice_radius: 40.0,
        practice_draft: None,
        practice_drag: None,
        practice_save: None,
        socket: None,
        socket_handlers: None,
        connection_epoch: 0,
        auth_epoch: 0,
        auth_pending: false,
        auth_request: None,
        event_handlers: Vec::new(),
        dynamic_event_handlers: Vec::new(),
        reconnect_attempt: 0,
        reconnect_timer: None,
        clock: None,
        viewport_handler: None,
        expired_deadline: None,
        shot_animation: None,
        soldier_sprite: None,
        soldier_sprite_loaded: false,
        soldier_helmet_team_one: None,
        soldier_helmet_team_one_loaded: false,
        soldier_helmet_team_two: None,
        soldier_helmet_team_two_loaded: false,
        soldier_sprite_handlers: Vec::new(),
        ws_url,
    }));

    load_soldier_sprites(&app)?;
    render(&app)?;
    bind_events(&app)?;
    bind_viewport_events(&app)?;
    let clock_app = Rc::clone(&app);
    app.borrow_mut().clock = Some(Interval::new(1_000, move || update_timer(&clock_app)));
    restore_session(&app);
    Ok(())
}

fn load_soldier_sprites(app: &SharedApp) -> Result<(), JsValue> {
    load_soldier_sprite(
        app,
        SoldierSpriteKind::Body,
        "/rsc/soldiers/soldierNormal.png?v=20260731d",
    )?;
    load_soldier_sprite(
        app,
        SoldierSpriteKind::HelmetTeamOne,
        "/rsc/soldiers/helmetTeamOne.png?v=20260731d",
    )?;
    load_soldier_sprite(
        app,
        SoldierSpriteKind::HelmetTeamTwo,
        "/rsc/soldiers/helmetTeamTwo.png?v=20260731d",
    )
}

fn load_soldier_sprite(
    app: &SharedApp,
    kind: SoldierSpriteKind,
    source: &str,
) -> Result<(), JsValue> {
    let image = HtmlImageElement::new()?;
    let load_app = Rc::clone(app);
    let onload = Closure::<dyn FnMut(Event)>::new(move |_| {
        let redraw = {
            let mut app = load_app.borrow_mut();
            set_soldier_sprite_loaded(&mut app, kind, true);
            app.model.screen == Screen::Game
        };
        if redraw && let Err(error) = render_canvas(&load_app) {
            log_error(&format!("soldier sprite render failed: {error:?}"));
        } else if load_app.borrow().model.room_kind == Some(RoomKind::Practice)
            && let Err(error) = render_practice_canvas(&load_app)
        {
            log_error(&format!("soldier sprite render failed: {error:?}"));
        }
    });
    image.set_onload(Some(onload.as_ref().unchecked_ref()));

    let error_app = Rc::clone(app);
    let onerror = Closure::<dyn FnMut(Event)>::new(move |_| {
        let redraw = {
            let mut app = error_app.borrow_mut();
            set_soldier_sprite_loaded(&mut app, kind, false);
            app.model.screen == Screen::Game
        };
        if redraw && let Err(error) = render_canvas(&error_app) {
            log_error(&format!("soldier sprite render failed: {error:?}"));
        }
    });
    image.set_onerror(Some(onerror.as_ref().unchecked_ref()));
    image.set_src(source);
    let loaded = image.complete() && image.natural_width() > 0;
    let mut app = app.borrow_mut();
    set_soldier_sprite(&mut app, kind, image, loaded);
    app.soldier_sprite_handlers.push(SoldierSpriteHandlers {
        _onload: onload,
        _onerror: onerror,
    });
    Ok(())
}

fn set_soldier_sprite(
    app: &mut App,
    kind: SoldierSpriteKind,
    image: HtmlImageElement,
    loaded: bool,
) {
    match kind {
        SoldierSpriteKind::Body => {
            app.soldier_sprite = Some(image);
            app.soldier_sprite_loaded = loaded;
        }
        SoldierSpriteKind::HelmetTeamOne => {
            app.soldier_helmet_team_one = Some(image);
            app.soldier_helmet_team_one_loaded = loaded;
        }
        SoldierSpriteKind::HelmetTeamTwo => {
            app.soldier_helmet_team_two = Some(image);
            app.soldier_helmet_team_two_loaded = loaded;
        }
    }
}

fn set_soldier_sprite_loaded(app: &mut App, kind: SoldierSpriteKind, loaded: bool) {
    match kind {
        SoldierSpriteKind::Body => app.soldier_sprite_loaded = loaded,
        SoldierSpriteKind::HelmetTeamOne => app.soldier_helmet_team_one_loaded = loaded,
        SoldierSpriteKind::HelmetTeamTwo => app.soldier_helmet_team_two_loaded = loaded,
    }
}

fn restore_session(app: &SharedApp) {
    let auth_epoch = app.borrow().auth_epoch;
    let app = Rc::clone(app);
    spawn_local(async move {
        match Request::get("/auth/me")
            .credentials(RequestCredentials::SameOrigin)
            .send()
            .await
        {
            Ok(response) if response.ok() => match response.json::<AccountResponse>().await {
                Ok(account) => restored_authenticated(&app, auth_epoch, account),
                Err(error) => log_error(&format!("account response failed: {error:?}")),
            },
            Ok(_) => {}
            Err(error) => log_error(&format!("session restore failed: {error:?}")),
        }
    });
}

fn restored_authenticated(app: &SharedApp, auth_epoch: u64, account: AccountResponse) {
    let previous_screen = {
        let mut app_ref = app.borrow_mut();
        if app_ref.auth_epoch != auth_epoch
            || app_ref.auth_pending
            || app_ref.model.player_id.is_some()
        {
            return;
        }
        let previous_screen = app_ref.model.screen.clone();
        reduce(
            &mut app_ref.model,
            Action::Authenticated {
                player_id: account.id.to_string(),
                display_name: account.display_name,
            },
        );
        previous_screen
    };
    redraw_after(app, previous_screen);
    if let Err(error) = connect(app) {
        notice(app, format!("connection failed: {error:?}"));
    }
}

fn websocket_url(window: &Window) -> Result<String, JsValue> {
    let location = window.location();
    let protocol = if location.protocol()? == "https:" {
        "wss"
    } else {
        "ws"
    };
    Ok(format!("{protocol}://{}/ws", location.host()?))
}

fn begin_authentication(app: &SharedApp) -> Option<(u64, AbortController)> {
    let controller = AbortController::new().ok()?;
    let (auth_epoch, socket, handlers) = {
        let mut app = app.borrow_mut();
        if app.auth_pending {
            return None;
        }
        app.auth_epoch = app.auth_epoch.saturating_add(1);
        app.auth_pending = true;
        app.auth_request = Some(controller.clone());
        app.connection_epoch = app.connection_epoch.saturating_add(1);
        app.practice_draft = None;
        app.practice_drag = None;
        app.practice_save = None;
        app.reconnect_timer = None;
        (
            app.auth_epoch,
            app.socket.take(),
            app.socket_handlers.take(),
        )
    };
    if let Some(socket) = socket {
        dispose_socket(socket, handlers);
    }
    Some((auth_epoch, controller))
}

fn finish_authentication_failure(app: &SharedApp, auth_epoch: u64) -> bool {
    let mut app = app.borrow_mut();
    if app.auth_epoch != auth_epoch || !app.auth_pending {
        return false;
    }
    app.auth_pending = false;
    app.auth_request = None;
    true
}

fn connection_is_current(app: &App, connection_epoch: u64, auth_epoch: u64) -> bool {
    app.connection_epoch == connection_epoch
        && app.auth_epoch == auth_epoch
        && !app.auth_pending
        && app.model.player_id.is_some()
}

fn dispose_socket(socket: WebSocket, handlers: Option<SocketHandlers>) {
    socket.set_onopen(None);
    socket.set_onmessage(None);
    socket.set_onclose(None);
    socket.set_onerror(None);
    drop(handlers);
    let _ = socket.close();
}

fn connect(app: &SharedApp) -> Result<(), JsValue> {
    let (ws_url, connection_epoch, auth_epoch, previous_socket, previous_handlers, previous_screen) = {
        let mut app = app.borrow_mut();
        if app.model.player_id.is_none() || app.auth_pending {
            return Ok(());
        }
        app.connection_epoch = app.connection_epoch.saturating_add(1);
        app.reconnect_timer = None;
        let previous_socket = app.socket.take();
        let previous_handlers = app.socket_handlers.take();
        let previous_screen = app.model.screen.clone();
        reduce(&mut app.model, Action::Connecting);
        (
            app.ws_url.clone(),
            app.connection_epoch,
            app.auth_epoch,
            previous_socket,
            previous_handlers,
            previous_screen,
        )
    };
    if let Some(socket) = previous_socket {
        dispose_socket(socket, previous_handlers);
    }
    redraw_scope(app, previous_screen, RenderScope::Header);
    let socket = match WebSocket::new(&ws_url) {
        Ok(socket) => socket,
        Err(error) => {
            let current = {
                let app_ref = app.borrow();
                app_ref.connection_epoch == connection_epoch && app_ref.auth_epoch == auth_epoch
            };
            if current {
                schedule_reconnect(app, connection_epoch, auth_epoch);
            }
            return Err(error);
        }
    };

    let open_app = Rc::clone(app);
    let onopen = Closure::<dyn FnMut(Event)>::new(move |_| {
        if !connection_is_current(&open_app.borrow(), connection_epoch, auth_epoch) {
            return;
        }
        {
            let mut app = open_app.borrow_mut();
            app.reconnect_timer = None;
        }
        send_current(
            &open_app,
            connection_epoch,
            auth_epoch,
            ClientMessage::Hello {
                version: PROTOCOL_VERSION,
            },
        );
    });
    socket.set_onopen(Some(onopen.as_ref().unchecked_ref()));

    let message_app = Rc::clone(app);
    let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        if !connection_is_current(&message_app.borrow(), connection_epoch, auth_epoch) {
            return;
        }
        let Some(text) = event.data().as_string() else {
            return;
        };
        match serde_json::from_str::<ServerMessage>(&text) {
            Ok(ServerMessage::SessionExpired) => session_expired(&message_app),
            Ok(ServerMessage::Hello { version }) if version == PROTOCOL_VERSION => {
                let previous_screen = {
                    let mut app = message_app.borrow_mut();
                    if !connection_is_current(&app, connection_epoch, auth_epoch) {
                        return;
                    }
                    app.reconnect_attempt = 0;
                    let previous_screen = app.model.screen.clone();
                    reduce(&mut app.model, Action::Connected);
                    previous_screen
                };
                redraw_scope(&message_app, previous_screen, RenderScope::Header);
            }
            Ok(ServerMessage::Hello { .. }) => {
                notice(
                    &message_app,
                    "Client update required; reload the page".into(),
                );
                dispose_current_socket(&message_app, connection_epoch, auth_epoch);
            }
            Ok(message) => {
                announce_server_message(&message_app, &message);
                let mut scope = render_scope(&message);
                let (previous_screen, prior_sequence, prior_notice) = {
                    let app_ref = message_app.borrow();
                    (
                        app_ref.model.screen.clone(),
                        app_ref.model.shot_sequence,
                        app_ref.model.notices.last().cloned(),
                    )
                };
                let authoritative_practice_update =
                    practice_authoritative_update(&message_app, &message);
                let queued = {
                    let mut app = message_app.borrow_mut();
                    let practice_update = practice_server_update(&message, &app);
                    reduce(&mut app.model, Action::Message(Box::new(message)));
                    match practice_update {
                        PracticeServerUpdate::Accepted => {
                            app.practice_save = None;
                            app.practice_drag = None;
                            let queued = app
                                .practice_draft
                                .as_ref()
                                .filter(|draft| Some(*draft) != app.model.practice_setup.as_ref())
                                .cloned();
                            if queued.is_none() {
                                app.practice_draft = None;
                            }
                            if app.model.screen == Screen::Room
                                && app.model.room_kind == Some(RoomKind::Practice)
                            {
                                scope = RenderScope::Screen;
                            }
                            queued
                        }
                        PracticeServerUpdate::Rejected => {
                            app.practice_save = None;
                            scope = RenderScope::Screen;
                            None
                        }
                        PracticeServerUpdate::Left => {
                            app.practice_draft = None;
                            app.practice_drag = None;
                            app.practice_save = None;
                            None
                        }
                        PracticeServerUpdate::None => None,
                    }
                };
                if let Some(queued) = queued {
                    submit_practice_setup(&message_app, queued);
                } else if authoritative_practice_update {
                    retry_practice_draft(&message_app);
                }
                let latest_notice = message_app.borrow().model.notices.last().cloned();
                if latest_notice != prior_notice
                    && let Some(message) = latest_notice.as_deref()
                {
                    announce(&message_app, message);
                }
                let sequence = message_app.borrow().model.shot_sequence;
                if sequence != prior_sequence {
                    start_shot_animation(&message_app, sequence);
                }
                redraw_scope(&message_app, previous_screen, scope);
            }
            Err(error) => log_error(&format!("protocol error: {error}")),
        }
    });
    socket.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));

    let close_app = Rc::clone(app);
    let onclose = Closure::<dyn FnMut(CloseEvent)>::new(move |_| {
        schedule_reconnect(&close_app, connection_epoch, auth_epoch);
    });
    socket.set_onclose(Some(onclose.as_ref().unchecked_ref()));

    let error_app = Rc::clone(app);
    let onerror = Closure::<dyn FnMut(ErrorEvent)>::new(move |event: ErrorEvent| {
        if connection_is_current(&error_app.borrow(), connection_epoch, auth_epoch) {
            log_error(&format!("WebSocket error: {}", event.message()));
        }
    });
    socket.set_onerror(Some(onerror.as_ref().unchecked_ref()));

    let mut app_ref = app.borrow_mut();
    if !connection_is_current(&app_ref, connection_epoch, auth_epoch) {
        drop(app_ref);
        dispose_socket(socket, None);
        return Ok(());
    }
    app_ref.socket = Some(socket);
    app_ref.socket_handlers = Some(SocketHandlers {
        _onopen: onopen,
        _onmessage: onmessage,
        _onclose: onclose,
        _onerror: onerror,
    });
    Ok(())
}

fn dispose_current_socket(app: &SharedApp, connection_epoch: u64, auth_epoch: u64) {
    let (socket, handlers) = {
        let mut app = app.borrow_mut();
        if !connection_is_current(&app, connection_epoch, auth_epoch) {
            return;
        }
        app.connection_epoch = app.connection_epoch.saturating_add(1);
        (app.socket.take(), app.socket_handlers.take())
    };
    if let Some(socket) = socket {
        dispose_socket(socket, handlers);
    }
}

fn schedule_reconnect(app: &SharedApp, connection_epoch: u64, auth_epoch: u64) {
    let (attempt, timer_epoch, previous_screen) = {
        let mut app = app.borrow_mut();
        if !connection_is_current(&app, connection_epoch, auth_epoch) {
            return;
        }
        app.socket = None;
        app.socket_handlers = None;
        app.practice_save = None;
        app.connection_epoch = app.connection_epoch.saturating_add(1);
        let timer_epoch = app.connection_epoch;
        let attempt = app.reconnect_attempt.saturating_add(1);
        app.reconnect_attempt = attempt;
        let previous_screen = app.model.screen.clone();
        reduce(&mut app.model, Action::Disconnected { attempt });
        (attempt, timer_epoch, previous_screen)
    };
    redraw_scope(app, previous_screen, RenderScope::Header);

    let cap_ms = 500_u32.saturating_mul(2_u32.saturating_pow(attempt.min(6)));
    let delay_ms = 500 + (js_sys::Math::random() * f64::from(cap_ms.saturating_sub(500))) as u32;
    let reconnect_app = Rc::clone(app);
    let timer = Timeout::new(delay_ms, move || {
        if !connection_is_current(&reconnect_app.borrow(), timer_epoch, auth_epoch) {
            return;
        }
        if let Err(error) = connect(&reconnect_app) {
            log_error(&format!("reconnect failed: {error:?}"));
            let (connection_epoch, auth_epoch) = {
                let app = reconnect_app.borrow();
                (app.connection_epoch, app.auth_epoch)
            };
            schedule_reconnect(&reconnect_app, connection_epoch, auth_epoch);
        }
    });
    let mut app = app.borrow_mut();
    if connection_is_current(&app, timer_epoch, auth_epoch) {
        app.reconnect_timer = Some(timer);
    }
}

fn send(app: &SharedApp, message: ClientMessage) -> bool {
    let (connection_epoch, auth_epoch, unavailable) = {
        let app = app.borrow();
        (
            app.connection_epoch,
            app.auth_epoch,
            !matches!(app.model.connection, Connection::Online)
                || app
                    .socket
                    .as_ref()
                    .is_none_or(|socket| socket.ready_state() != WebSocket::OPEN),
        )
    };
    if unavailable {
        notice(app, "Connection unavailable; action not sent".into());
        return false;
    }
    send_current(app, connection_epoch, auth_epoch, message)
}

fn send_current(
    app: &SharedApp,
    connection_epoch: u64,
    auth_epoch: u64,
    message: ClientMessage,
) -> bool {
    let result = serde_json::to_string(&message)
        .map_err(|error| JsValue::from_str(&error.to_string()))
        .and_then(|json| {
            let app = app.borrow();
            if !connection_is_current(&app, connection_epoch, auth_epoch) {
                return Err(JsValue::from_str("connection unavailable"));
            }
            app.socket
                .as_ref()
                .ok_or_else(|| JsValue::from_str("connection unavailable"))?
                .send_with_str(&json)
        });
    if let Err(error) = result {
        log_error(&format!("send failed: {error:?}"));
        notice(app, "Connection failed; action not sent".into());
        false
    } else {
        true
    }
}

fn login(app: &SharedApp, request: LoginRequest) {
    let Some((auth_epoch, controller)) = begin_authentication(app) else {
        return;
    };
    let app = Rc::clone(app);
    spawn_local(async move {
        match post_json("/auth/login", &request, &controller).await {
            Ok(account) => authenticated(&app, auth_epoch, account),
            Err(message) if finish_authentication_failure(&app, auth_epoch) => {
                notice(&app, message)
            }
            Err(_) => {}
        }
    });
}

fn register(app: &SharedApp, request: RegisterRequest) {
    let Some((auth_epoch, controller)) = begin_authentication(app) else {
        return;
    };
    let app = Rc::clone(app);
    spawn_local(async move {
        match post_json("/auth/register", &request, &controller).await {
            Ok(account) => {
                let login = LoginRequest {
                    email: account.email,
                    password: request.password,
                };
                match post_json("/auth/login", &login, &controller).await {
                    Ok(account) => authenticated(&app, auth_epoch, account),
                    Err(message) if finish_authentication_failure(&app, auth_epoch) => {
                        notice(&app, message)
                    }
                    Err(_) => {}
                }
            }
            Err(message) if finish_authentication_failure(&app, auth_epoch) => {
                notice(&app, message)
            }
            Err(_) => {}
        }
    });
}

fn authenticated(app: &SharedApp, auth_epoch: u64, account: AccountResponse) {
    let previous_screen = {
        let mut app_ref = app.borrow_mut();
        if app_ref.auth_epoch != auth_epoch || !app_ref.auth_pending {
            return;
        }
        app_ref.auth_pending = false;
        app_ref.auth_request = None;
        let previous_screen = app_ref.model.screen.clone();
        reduce(
            &mut app_ref.model,
            Action::Authenticated {
                player_id: account.id.to_string(),
                display_name: account.display_name,
            },
        );
        previous_screen
    };
    redraw_after(app, previous_screen);
    if let Err(error) = connect(app) {
        notice(app, format!("connection failed: {error:?}"));
    }
}

fn session_expired(app: &SharedApp) {
    let (socket, handlers, previous_screen) = {
        let mut app_ref = app.borrow_mut();
        app_ref.auth_epoch = app_ref.auth_epoch.saturating_add(1);
        app_ref.auth_pending = false;
        app_ref.connection_epoch = app_ref.connection_epoch.saturating_add(1);
        app_ref.reconnect_timer = None;
        app_ref.auth_request = None;
        let socket = app_ref.socket.take();
        let handlers = app_ref.socket_handlers.take();
        let previous_screen = app_ref.model.screen.clone();
        reduce(&mut app_ref.model, Action::SessionExpired);
        app_ref.practice_draft = None;
        app_ref.practice_drag = None;
        app_ref.practice_save = None;
        (socket, handlers, previous_screen)
    };
    if let Some(socket) = socket {
        dispose_socket(socket, handlers);
    }
    redraw_after(app, previous_screen);
    notice(app, "Session expired; sign in again".into());
}

fn logout(app: &SharedApp) {
    let (auth_epoch, request, socket, handlers, previous_screen) = {
        let mut app_ref = app.borrow_mut();
        app_ref.auth_epoch = app_ref.auth_epoch.saturating_add(1);
        app_ref.auth_pending = false;
        app_ref.connection_epoch = app_ref.connection_epoch.saturating_add(1);
        app_ref.reconnect_timer = None;
        let request = app_ref.auth_request.take();
        let socket = app_ref.socket.take();
        let handlers = app_ref.socket_handlers.take();
        let previous_screen = app_ref.model.screen.clone();
        reduce(&mut app_ref.model, Action::LoggedOut);
        app_ref.practice_draft = None;
        app_ref.practice_drag = None;
        app_ref.practice_save = None;
        (
            app_ref.auth_epoch,
            request,
            socket,
            handlers,
            previous_screen,
        )
    };
    if let Some(request) = request {
        request.abort();
    }
    if let Some(socket) = socket {
        dispose_socket(socket, handlers);
    }
    redraw_after(app, previous_screen);
    let app = Rc::clone(app);
    spawn_local(async move {
        let result = Request::post("/auth/logout")
            .credentials(RequestCredentials::SameOrigin)
            .send()
            .await;
        if app.borrow().auth_epoch != auth_epoch {
            return;
        }
        match result {
            Ok(response) if response.ok() => {}
            Ok(_) => notice(&app, "logout failed".into()),
            Err(error) => notice(&app, format!("logout failed: {error:?}")),
        }
    });
}

async fn post_json<T: serde::Serialize>(
    path: &str,
    body: &T,
    controller: &AbortController,
) -> Result<AccountResponse, String> {
    let body = serde_json::to_string(body).map_err(|error| error.to_string())?;
    let response = Request::post(path)
        .credentials(RequestCredentials::SameOrigin)
        .abort_signal(Some(&controller.signal()))
        .header("content-type", "application/json")
        .body(body)
        .map_err(|error| format!("request setup failed: {error:?}"))?
        .send()
        .await
        .map_err(|error| format!("request failed: {error:?}"))?;
    if !response.ok() {
        return Err(response
            .text()
            .await
            .unwrap_or_else(|_| "authentication failed".into()));
    }
    response
        .json()
        .await
        .map_err(|error| format!("invalid account response: {error:?}"))
}

fn announce(app: &SharedApp, message: &str) {
    if let Some(region) = app.borrow().document.get_element_by_id("announcements") {
        region.set_text_content(None);
        region.set_text_content(Some(message));
    }
}

fn announce_server_message(app: &SharedApp, message: &ServerMessage) {
    let message = match message {
        ServerMessage::Chat { entry } => Some(format!("{}: {}", entry.display_name, entry.text)),
        ServerMessage::Room { snapshot, .. } => snapshot.players.iter().find_map(|player| {
            app.borrow()
                .model
                .players
                .iter()
                .find(|current| current.id == player.id.to_string() && current.team != player.team)
                .map(|_| {
                    format!(
                        "{} moved to {}",
                        player.display_name,
                        team_name(player.team)
                    )
                })
        }),
        ServerMessage::GameStarted { snapshot, .. } => {
            Some(format!("Match started in {}", snapshot.name))
        }
        ServerMessage::TurnStarted { game, .. } => game
            .turn_player_id
            .and_then(|player_id| {
                app.borrow()
                    .model
                    .players
                    .iter()
                    .find(|player| player.id == player_id.to_string())
                    .map(|player| player.name.clone())
            })
            .map(|name| format!("{name}'s turn")),
        ServerMessage::ShotResolved { shot, .. } => Some(shot_outcome_message(&shot.outcome)),
        ServerMessage::GameFinished { shot, .. } => Some(match shot.winner_team {
            Some(1) => "Match finished; Team One wins".into(),
            Some(2) => "Match finished; Team Two wins".into(),
            _ => "Match finished; draw".into(),
        }),
        _ => None,
    };
    if let Some(message) = message {
        announce(app, &message);
    }
}

fn shot_outcome_message(outcome: &ShotOutcome) -> String {
    match outcome {
        ShotOutcome::TerrainImpact { hits, .. } if hits.is_empty() => {
            "Terrain hit; no soldiers caught in the blast".into()
        }
        ShotOutcome::TerrainImpact { hits, .. } => {
            format!("Terrain hit; {} soldier(s) caught in the blast", hits.len())
        }
        ShotOutcome::Miss {
            reason: ShotMissReason::WorldExit,
        } => "Shot missed: trajectory left the battlefield".into(),
        ShotOutcome::Miss {
            reason: ShotMissReason::Numerical,
        } => "Shot missed: function became undefined".into(),
        ShotOutcome::Miss {
            reason: ShotMissReason::StepLimit,
        } => "Shot missed: simulation limit reached".into(),
        ShotOutcome::Forfeit => "Player forfeited".into(),
    }
}

fn notice(app: &SharedApp, message: String) {
    announce(app, &message);
    let previous_screen = {
        let mut app = app.borrow_mut();
        app.model.notices.push(message);
        if app.model.notices.len() > 40 {
            app.model.notices.remove(0);
        }
        app.model.screen.clone()
    };
    redraw_scope(app, previous_screen, RenderScope::Notices);
}

fn render_scope(message: &ServerMessage) -> RenderScope {
    match message {
        ServerMessage::Hello { .. } => RenderScope::None,
        ServerMessage::RoomList { .. } => RenderScope::Rooms,
        ServerMessage::Chat { .. } => RenderScope::Chat,
        ServerMessage::Error { .. } => RenderScope::Notices,
        ServerMessage::SessionExpired => RenderScope::None,
        ServerMessage::RoomCreated { .. }
        | ServerMessage::Room { .. }
        | ServerMessage::GameStarted { .. }
        | ServerMessage::TurnStarted { .. }
        | ServerMessage::ShotResolved { .. }
        | ServerMessage::GameFinished { .. }
        | ServerMessage::StateSync { .. }
        | ServerMessage::LeftRoom => RenderScope::Screen,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum PracticeServerUpdate {
    None,
    Accepted,
    Rejected,
    Left,
}

fn practice_server_update(message: &ServerMessage, app: &App) -> PracticeServerUpdate {
    let Some(pending) = app.practice_save.as_ref() else {
        return if matches!(message, ServerMessage::LeftRoom) {
            PracticeServerUpdate::Left
        } else {
            PracticeServerUpdate::None
        };
    };
    match message {
        ServerMessage::RoomCreated {
            snapshot,
            practice_setup,
            ..
        }
        | ServerMessage::Room {
            snapshot,
            practice_setup,
        }
        | ServerMessage::StateSync {
            snapshot,
            practice_setup,
            ..
        } if snapshot.id.to_string() == pending.room_id
            && snapshot.revision > pending.base_revision
            && practice_setup.as_ref() == Some(&pending.submitted) =>
        {
            PracticeServerUpdate::Accepted
        }
        ServerMessage::Error { .. } => PracticeServerUpdate::Rejected,
        ServerMessage::LeftRoom => PracticeServerUpdate::Left,
        _ => PracticeServerUpdate::None,
    }
}

fn practice_authoritative_update(app: &SharedApp, message: &ServerMessage) -> bool {
    let Some(room_id) = app.borrow().model.room_id.clone() else {
        return false;
    };
    match message {
        ServerMessage::RoomCreated { snapshot, .. }
        | ServerMessage::Room { snapshot, .. }
        | ServerMessage::StateSync { snapshot, .. } => snapshot.id.to_string() == room_id,
        _ => false,
    }
}

fn retry_practice_draft(app: &SharedApp) {
    let draft = {
        let app = app.borrow();
        app.practice_draft
            .as_ref()
            .filter(|draft| Some(*draft) != app.model.practice_setup.as_ref())
            .filter(|draft| validate_practice_setup_client(draft).is_ok())
            .cloned()
    };
    if let Some(draft) = draft {
        submit_practice_setup(app, draft);
    } else if app.borrow().model.room_kind == Some(RoomKind::Practice) {
        app.borrow_mut().practice_draft = None;
        let status = {
            let app = app.borrow();
            practice_setup_status(app.model.practice_setup.as_ref())
        };
        practice_status(app, status);
    }
}

fn redraw_after(app: &SharedApp, previous_screen: Screen) {
    redraw_scope(app, previous_screen, RenderScope::Screen);
}

fn redraw_scope(app: &SharedApp, previous_screen: Screen, scope: RenderScope) {
    let screen_changed = { app.borrow().model.screen != previous_screen };
    if screen_changed {
        rerender(app);
        return;
    }
    let result = match scope {
        RenderScope::None => Ok(()),
        RenderScope::Header => refresh_header_dom(app),
        RenderScope::Notices => refresh_notices_dom(app),
        RenderScope::Rooms => {
            if previous_screen == Screen::Lobby {
                refresh_lobby_rooms_dom(app)
            } else {
                Ok(())
            }
        }
        RenderScope::Chat => refresh_chat_dom(app),
        RenderScope::Screen => refresh_current_dom(app),
    };
    if let Err(error) = result {
        log_error(&format!("partial refresh failed: {error:?}"));
        rerender(app);
    }
}

fn rerender(app: &SharedApp) {
    let form_state = capture_form_state(app);
    if app.borrow().model.screen != Screen::Room {
        app.borrow_mut().selected_team_player = None;
    }
    if let Err(error) = render(app) {
        log_error(&format!("render failed: {error:?}"));
        return;
    }
    restore_form_state(app, form_state);
    {
        let mut app = app.borrow_mut();
        app.event_handlers.clear();
        app.dynamic_event_handlers.clear();
    }
    if let Err(error) = bind_events(app) {
        log_error(&format!("event binding failed: {error:?}"));
    }
}

fn refresh_current_dom(app: &SharedApp) -> Result<(), JsValue> {
    refresh_header_dom(app)?;
    let screen = { app.borrow().model.screen.clone() };
    match screen {
        Screen::Login => refresh_notices_dom(app),
        Screen::Lobby => {
            refresh_lobby_rooms_dom(app)?;
            refresh_notices_dom(app)
        }
        Screen::Room => refresh_room_dom(app),
        Screen::Game => refresh_game_dom(app),
    }
}

fn refresh_header_dom(app: &SharedApp) -> Result<(), JsValue> {
    let app_ref = app.borrow();
    let connection = app_ref
        .document
        .query_selector(".connection")?
        .ok_or(".connection missing")?;
    let reconnect = app_ref
        .document
        .get_element_by_id("reconnect-now")
        .ok_or("#reconnect-now missing")?;
    let (class, label) = connection_view(&app_ref.model.connection);
    connection.set_class_name(&format!("connection {class}"));
    connection.set_inner_html(&format!("<i></i>{}", escape(&label)));
    set_boolean_attribute(
        &reconnect,
        "hidden",
        !matches!(app_ref.model.connection, Connection::Offline),
    )
}

fn refresh_notices_dom(app: &SharedApp) -> Result<(), JsValue> {
    let app_ref = app.borrow();
    if app_ref.model.screen == Screen::Game {
        return Ok(());
    }
    let notices = app_ref
        .document
        .query_selector(".app-notices")?
        .ok_or(".app-notices missing")?;
    notices.set_inner_html(&notice_items_html(&app_ref.model));
    Ok(())
}

fn refresh_lobby_rooms_dom(app: &SharedApp) -> Result<(), JsValue> {
    let (rooms, items) = {
        let app_ref = app.borrow();
        if app_ref.model.screen != Screen::Lobby {
            return Err(JsValue::from_str("lobby screen unavailable"));
        }
        (
            app_ref.document.clone(),
            lobby_room_items_html(&app_ref.model),
        )
    };
    let list = rooms
        .query_selector(".room-list")?
        .ok_or(".room-list missing")?;
    list.set_inner_html(&items);
    app.borrow_mut().dynamic_event_handlers.clear();
    bind_lobby_room_events(app, &rooms)
}

fn refresh_room_dom(app: &SharedApp) -> Result<(), JsValue> {
    let form_state = capture_form_state(app);
    {
        let app_ref = app.borrow();
        let model = &app_ref.model;
        if model.screen != Screen::Room {
            return Err(JsValue::from_str("room screen unavailable"));
        }
        if model.room_kind == Some(RoomKind::Practice) {
            let has_toolbar = app_ref
                .document
                .query_selector(".practice-toolbar")?
                .is_some();
            if has_toolbar != model.local_owner() {
                drop(app_ref);
                rerender(app);
                return Ok(());
            }
        }
    }
    let document = {
        let app_ref = app.borrow();
        let document = &app_ref.document;
        let model = &app_ref.model;
        room_element(document, "#room-title")?.set_text_content(Some(&model.room_name));
        room_element(document, "#players-title span")?
            .set_text_content(Some(&model.players.len().to_string()));
        room_element(document, ".team-rosters")?.set_inner_html(&room_team_rosters_html(model));
        room_element(document, ".room-chat ul")?.set_inner_html(&chat_messages_html(model));

        let owner = model.local_owner();
        set_boolean_attribute(&room_element(document, ".mode-picker")?, "disabled", !owner)?;
        let mode = model.game_mode.unwrap_or(GameMode::Function);
        let mode_inputs = document.query_selector_all("input[name=game-mode]")?;
        for index in 0..mode_inputs.length() {
            let Some(input) = mode_inputs.item(index) else {
                continue;
            };
            let input = input.dyn_into::<HtmlInputElement>()?;
            input.set_checked(
                matches!(input.value().as_str(), "function") && mode == GameMode::Function
                    || matches!(input.value().as_str(), "first_order")
                        && mode == GameMode::FirstOrder
                    || matches!(input.value().as_str(), "second_order")
                        && mode == GameMode::SecondOrder,
            );
        }
        room_element(document, "#ready-button")?.set_text_content(Some(if model.local_ready() {
            "Not ready"
        } else {
            "I’m ready"
        }));
        set_boolean_attribute(&room_element(document, "#add-bot")?, "disabled", !owner)?;
        set_boolean_attribute(
            &room_element(document, "#start-game")?,
            "disabled",
            !model.can_start(),
        )?;
        document.clone()
    };
    refresh_notices_dom(app)?;
    if let Some(precise) = document.query_selector(".practice-precise")? {
        let open = precise.has_attribute("open");
        let precise_html = {
            let app_ref = app.borrow();
            app_ref
                .model
                .local_owner()
                .then(|| precise_placement_html(&app_ref.model, open))
        };
        if let Some(precise_html) = precise_html {
            precise.set_outer_html(&precise_html);
        }
    }
    app.borrow_mut().dynamic_event_handlers.clear();
    bind_room_roster_events(app, &document)?;
    if app.borrow().model.room_kind == Some(RoomKind::Practice) {
        bind_practice_events(app, &document)?;
        render_practice_canvas(app)?;
    }
    restore_form_state(app, form_state);
    Ok(())
}

fn refresh_chat_dom(app: &SharedApp) -> Result<(), JsValue> {
    let app_ref = app.borrow();
    let selector = match app_ref.model.screen {
        Screen::Room => ".room-chat ul",
        Screen::Game => ".game-chat ul",
        Screen::Login | Screen::Lobby => return Ok(()),
    };
    let list = app_ref
        .document
        .query_selector(selector)?
        .ok_or_else(|| JsValue::from_str(&format!("{selector} missing")))?;
    list.set_inner_html(&chat_messages_html(&app_ref.model));
    Ok(())
}

fn room_element(document: &Document, selector: &str) -> Result<web_sys::Element, JsValue> {
    document
        .query_selector(selector)?
        .ok_or_else(|| JsValue::from_str(&format!("{selector} missing")))
}

fn refresh_game(app: &SharedApp) {
    if let Err(error) = refresh_game_dom(app) {
        log_error(&format!("game refresh failed: {error:?}"));
        rerender(app);
    }
}

fn refresh_game_dom(app: &SharedApp) -> Result<(), JsValue> {
    let preview_input = {
        let app_ref = app.borrow();
        let document = &app_ref.document;
        let model = &app_ref.model;
        if model.screen != Screen::Game {
            return Err(JsValue::from_str("game screen unavailable"));
        }

        let timer = timer_view(model);
        game_element(document, "#turn-timer")?.set_text_content(Some(&timer.text));
        refresh_soldier_name_labels(document, model)?;
        game_element(document, "#battlefield-summary")?
            .set_text_content(Some(&battlefield_summary(model)));
        game_element(document, "#shot-status")?.set_text_content(model.shot_status.as_deref());
        game_element(document, ".game-chat ul")?.set_inner_html(&chat_messages_html(model));
        let finished = model.room_phase == Some(Phase::Finished);
        set_boolean_attribute(
            &game_element(document, ".finished-actions")?,
            "hidden",
            !finished,
        )?;
        set_boolean_attribute(
            &game_element(document, "#return-to-lobby")?,
            "disabled",
            !model.local_owner(),
        )?;

        let local_turn = local_turn(model);
        let function_input =
            game_element(document, "#function-input")?.dyn_into::<HtmlInputElement>()?;
        let start_preview =
            function_input.disabled() && local_turn && model.preview_path.is_empty();
        function_input.set_disabled(!local_turn);

        let second_order = model.game_mode == Some(GameMode::SecondOrder);
        set_boolean_attribute(
            &game_element(document, ".angle-field")?,
            "hidden",
            !second_order,
        )?;
        game_element(document, "#angle-input")?
            .dyn_into::<HtmlInputElement>()?
            .set_disabled(!second_order || !local_turn);
        let fire_button = game_element(document, ".fire-button")?;
        set_boolean_attribute(&fire_button, "disabled", !local_turn)?;
        set_turn_progress(&fire_button, timer.progress)?;
        game_element(document, ".fire-button-label")?
            .set_text_content(Some(&fire_label(model, local_turn)));

        start_preview.then_some(function_input)
    };

    if let Some(input) = preview_input {
        update_preview(app, &input);
        Ok(())
    } else {
        render_canvas(app)
    }
}

fn game_element(document: &Document, selector: &str) -> Result<web_sys::Element, JsValue> {
    document
        .query_selector(selector)?
        .ok_or_else(|| JsValue::from_str(&format!("{selector} missing")))
}

fn set_boolean_attribute(
    element: &web_sys::Element,
    name: &str,
    enabled: bool,
) -> Result<(), JsValue> {
    if enabled {
        element.set_attribute(name, "")
    } else {
        element.remove_attribute(name)
    }
}

fn capture_form_state(app: &SharedApp) -> FormState {
    let app = app.borrow();
    let inputs = PRESERVED_INPUTS
        .iter()
        .filter_map(|id| {
            app.document
                .get_element_by_id(id)?
                .dyn_into::<HtmlInputElement>()
                .ok()
                .map(|input| ((*id).to_owned(), input.value()))
        })
        .collect();
    let selects = PRESERVED_SELECTS
        .iter()
        .filter_map(|id| {
            app.document
                .get_element_by_id(id)?
                .dyn_into::<HtmlSelectElement>()
                .ok()
                .map(|select| ((*id).to_owned(), select.value()))
        })
        .collect();
    let focus = app.document.active_element().and_then(|element| {
        let selector = focus_selector(&element)?;
        let input = element.dyn_ref::<HtmlInputElement>();
        Some((
            selector,
            input.and_then(|input| input.selection_start().ok().flatten()),
            input.and_then(|input| input.selection_end().ok().flatten()),
            input.and_then(|input| input.selection_direction().ok().flatten()),
        ))
    });
    FormState {
        inputs,
        selects,
        focus,
    }
}

fn focus_selector(element: &web_sys::Element) -> Option<String> {
    let id = element.id();
    if !id.is_empty() {
        return Some(format!("#{id}"));
    }
    if let Some(player_id) = element.get_attribute("data-player-id") {
        let class = element
            .get_attribute("class")
            .and_then(|classes| classes.split_ascii_whitespace().next().map(str::to_owned))
            .map(|class| format!(".{class}"))
            .unwrap_or_default();
        return Some(format!(
            "{}{class}[data-player-id=\"{player_id}\"]",
            element.tag_name().to_ascii_lowercase()
        ));
    }
    let input = element.dyn_ref::<HtmlInputElement>()?;
    input
        .get_attribute("name")
        .map(|name| format!("input[name=\"{name}\"][value=\"{}\"]", input.value()))
}

fn restore_form_state(app: &SharedApp, state: FormState) {
    let document = app.borrow().document.clone();
    for (id, value) in state.inputs {
        if let Some(input) = document
            .get_element_by_id(&id)
            .and_then(|element| element.dyn_into::<HtmlInputElement>().ok())
        {
            input.set_value(&value);
        }
    }
    for (id, value) in state.selects {
        if let Some(select) = document
            .get_element_by_id(&id)
            .and_then(|element| element.dyn_into::<HtmlSelectElement>().ok())
        {
            select.set_value(&value);
        }
    }
    let private = document
        .get_element_by_id("room-visibility")
        .and_then(|element| element.dyn_into::<HtmlSelectElement>().ok())
        .is_some_and(|select| select.value() == "private");
    set_create_room_password_visibility(&document, private);
    if let Some((selector, start, end, direction)) = state.focus
        && let Ok(Some(element)) = document.query_selector(&selector)
    {
        if let Some(input) = element.dyn_ref::<HtmlInputElement>() {
            let _ = input.focus();
            if let (Some(start), Some(end)) = (start, end) {
                let _ = input.set_selection_range_with_direction(
                    start,
                    end,
                    direction.as_deref().unwrap_or("none"),
                );
            }
        } else if let Ok(element) = element.dyn_into::<web_sys::HtmlElement>() {
            let _ = element.focus();
        }
    }
}

fn render(app: &SharedApp) -> Result<(), JsValue> {
    let app_ref = app.borrow();
    let root = app_ref
        .document
        .get_element_by_id("app")
        .ok_or("#app missing")?;
    let screen = match app_ref.model.screen {
        Screen::Login => login_html(),
        Screen::Lobby => lobby_html(&app_ref.model),
        Screen::Room => room_html(&app_ref.model),
        Screen::Game => game_html(&app_ref.model),
    };
    let notices = (app_ref.model.screen != Screen::Game)
        .then(|| notices_html(&app_ref.model))
        .unwrap_or_default();
    root.set_inner_html(&format!(
        "<div class=\"app-frame\">{}<main id=\"screen\">{}</main>{}</div>",
        header_html(&app_ref.model),
        screen,
        notices
    ));
    drop(app_ref);
    let model = &app.borrow().model;
    let canvas_result = match (model.screen.clone(), model.room_kind) {
        (Screen::Game, _) => render_canvas(app),
        (Screen::Room, Some(RoomKind::Practice)) => render_practice_canvas(app),
        _ => Ok(()),
    };
    if let Err(error) = canvas_result {
        log_error(&format!("canvas render failed: {error:?}"));
    }
    Ok(())
}

fn header_html(model: &Model) -> String {
    let account_action = model
        .player_id
        .as_ref()
        .map(|_| "<button id=\"logout\" class=\"text-button\" type=\"button\">Log out</button>")
        .unwrap_or("");
    let (class, label) = connection_view(&model.connection);
    let reconnect_hidden = (!matches!(model.connection, Connection::Offline))
        .then_some(" hidden")
        .unwrap_or("");
    format!(
        "<header class=\"masthead\"><a class=\"wordmark\" href=\"/\" aria-label=\"Graphwar home\"><span>GRAPH</span><strong>WAR</strong></a><div class=\"masthead-status\"><p class=\"connection {class}\" role=\"status\"><i></i>{}</p><button id=\"reconnect-now\" class=\"text-button\" type=\"button\"{reconnect_hidden}>Reconnect</button>{account_action}</div></header>",
        escape(&label)
    )
}

fn connection_view(connection: &Connection) -> (&'static str, String) {
    match connection {
        Connection::Connecting => ("is-waiting", "Connecting".into()),
        Connection::Online => ("is-online", "Online".into()),
        Connection::Reconnecting { attempt } => {
            ("is-waiting", format!("Reconnecting · attempt {attempt}"))
        }
        Connection::Offline => ("is-offline", "Offline".into()),
    }
}

fn login_html() -> String {
    "<section class=\"login-shell reveal\" aria-labelledby=\"login-title\"><div class=\"hero-copy\"><p class=\"eyebrow\">Artillery for mathematicians</p><h1 id=\"login-title\">Draw the<br><em>winning line.</em></h1><p>Turn equations into trajectories. Outsmart the other side before the clock runs dry.</p></div><div class=\"auth-stack\"><form id=\"login-form\" class=\"paper-card auth-card auth-card-login\"><h2>Return to battle</h2><label for=\"login-email\">Email</label><input id=\"login-email\" type=\"email\" autocomplete=\"email\" maxlength=\"254\" required><label for=\"login-password\">Password</label><input id=\"login-password\" type=\"password\" autocomplete=\"current-password\" minlength=\"12\" required><button class=\"primary\" type=\"submit\">Enter the lobby <span aria-hidden=\"true\">↗</span></button></form><form id=\"register-form\" class=\"paper-card auth-card auth-card-register\"><h2>First deployment</h2><label for=\"register-name\">Display name</label><input id=\"register-name\" autocomplete=\"nickname\" minlength=\"2\" maxlength=\"32\" required placeholder=\"e.g. Gauss\"><label for=\"register-email\">Email</label><input id=\"register-email\" type=\"email\" autocomplete=\"email\" maxlength=\"254\" required><label for=\"register-password\">Password</label><input id=\"register-password\" type=\"password\" autocomplete=\"new-password\" minlength=\"12\" required><button class=\"secondary\" type=\"submit\">Create account</button><small>Passwords need at least 12 characters.</small></form></div></section>".into()
}

fn lobby_html(model: &Model) -> String {
    format!(
        "<section class=\"lobby-shell reveal\" aria-labelledby=\"lobby-title\"><div class=\"section-heading\"><div><p class=\"eyebrow\">Welcome, {}</p><h1 id=\"lobby-title\">Rooms</h1></div><div class=\"lobby-actions\"><button id=\"create-room-open\" class=\"primary\" type=\"button\" aria-haspopup=\"dialog\" aria-controls=\"create-room-dialog\">Create room</button></div></div><dialog id=\"create-room-dialog\" class=\"create-room-dialog\" aria-labelledby=\"create-room-title\"><form id=\"create-room-form\" class=\"command-slip\"><h2 id=\"create-room-title\">Create room</h2><label for=\"room-name\">Room name</label><input id=\"room-name\" maxlength=\"32\" required autocomplete=\"off\"><label for=\"room-visibility\">Visibility</label><select id=\"room-visibility\"><option value=\"public\">Public</option><option value=\"private\">Private</option></select><label for=\"room-kind\">Battle type</label><select id=\"room-kind\"><option value=\"standard\">Standard</option><option value=\"practice\">Practice</option></select><div id=\"room-password-field\" hidden><label for=\"room-password\">Password</label><input id=\"room-password\" type=\"password\" autocomplete=\"new-password\" maxlength=\"1024\" disabled></div><div class=\"create-room-actions\"><button id=\"create-room-cancel\" class=\"secondary\" type=\"button\">Cancel</button><button class=\"primary\" type=\"submit\">Create room</button></div></form></dialog><ul class=\"room-list\">{}</ul></section>",
        escape(&model.player_name),
        lobby_room_items_html(model)
    )
}

fn lobby_room_items_html(model: &Model) -> String {
    if model.rooms.is_empty() {
        return "<li class=\"empty\"><strong>No open rooms.</strong><span>Start the first skirmish.</span></li>".into();
    }
    model
        .rooms
        .iter()
        .map(|room| {
            let lock = room
                .protected
                .then_some("<span class=\"room-lock\"><svg aria-hidden=\"true\" viewBox=\"0 0 24 24\" width=\"18\" height=\"18\"><path d=\"M7 10V7a5 5 0 0 1 10 0v3m-11 0h12v10H6z\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/></svg><span class=\"sr-only\">Protected room</span></span>")
                .unwrap_or("");
            let protected = if room.protected { "true" } else { "false" };
            let practice = (room.kind == RoomKind::Practice)
                .then_some("<span class=\"room-kind\">Practice</span>")
                .unwrap_or("");
            let join_label = if room.protected {
                format!("Join protected room {}", room.name)
            } else {
                format!("Join room {}", room.name)
            };
            format!(
                "<li class=\"room-card\"><div><span class=\"room-card-title\"><strong>{}</strong>{lock}{practice}</span><span>{} / {} players</span></div><button class=\"join-room secondary\" data-room-id=\"{}\" data-room-protected=\"{protected}\" aria-label=\"{}\">Join <span aria-hidden=\"true\">→</span></button></li>",
                escape(&room.name),
                room.players,
                room.capacity,
                attr(&room.id),
                attr(&join_label),
            )
        })
        .collect()
}

fn room_html(model: &Model) -> String {
    let ready_label = if model.local_ready() {
        "Not ready"
    } else {
        "I’m ready"
    };
    let start_disabled = (!model.can_start()).then_some(" disabled").unwrap_or("");
    let owner_controls = model.local_owner();
    let mode = model.game_mode.unwrap_or(GameMode::Function);
    let mode_checked = |candidate| (mode == candidate).then_some(" checked").unwrap_or("");
    let practice = (model.room_kind == Some(RoomKind::Practice))
        .then(|| practice_editor_html(model))
        .unwrap_or_default();
    format!(
        "<section class=\"room-shell reveal\" aria-labelledby=\"room-title\"><div class=\"section-heading\"><div><p class=\"eyebrow\">Staging area</p><h1 id=\"room-title\">{}</h1></div><button id=\"leave-room\" class=\"text-button\">Leave room</button></div><div class=\"room-grid\"><section class=\"paper-card roster\" aria-labelledby=\"players-title\"><h2 id=\"players-title\">Players <span>{}</span></h2><div class=\"team-rosters\">{}</div><p id=\"roster-move-status\" class=\"sr-only\" aria-live=\"polite\"></p></section><aside class=\"briefing paper-card command-brief\"><p>Configure your slot, then ready up. The owner starts after everyone commits.</p><fieldset class=\"mode-picker\"{}><legend>Rule set</legend><label><input type=\"radio\" name=\"game-mode\" value=\"function\"{}> Function</label><label><input type=\"radio\" name=\"game-mode\" value=\"first_order\"{}> First-order</label><label><input type=\"radio\" name=\"game-mode\" value=\"second_order\"{}> Second-order</label></fieldset><button id=\"ready-button\" class=\"primary wide\">{ready_label}</button><button id=\"add-bot\" class=\"text-button wide\"{}>Add computer</button><button id=\"start-game\" class=\"secondary wide\"{}>Start match</button></aside></div>{practice}</section>{}",
        escape(&model.room_name),
        model.players.len(),
        room_team_rosters_html(model),
        if owner_controls { "" } else { " disabled" },
        mode_checked(GameMode::Function),
        mode_checked(GameMode::FirstOrder),
        mode_checked(GameMode::SecondOrder),
        if owner_controls { "" } else { " disabled" },
        start_disabled,
        chat_html(model, "room-chat")
    )
}

fn practice_setup_status(setup: Option<&PracticeSetup>) -> &'static str {
    if setup.is_some_and(|setup| setup.terrain.is_empty()) {
        "Blank terrain is valid. Add circles or start as-is."
    } else {
        "Setup synchronized."
    }
}

fn practice_editor_html(model: &Model) -> String {
    let owner = model.local_owner();
    let controls = if owner {
        "<div class=\"practice-toolbar\" role=\"toolbar\" aria-label=\"Battlefield tools\"><button type=\"button\" class=\"practice-tool secondary\" data-tool=\"move\" aria-pressed=\"true\">Move soldiers</button><button type=\"button\" class=\"practice-tool secondary\" data-tool=\"add\" aria-pressed=\"false\">Place terrain</button><button type=\"button\" class=\"practice-tool secondary\" data-tool=\"erase\" aria-pressed=\"false\">Erase terrain</button><label for=\"practice-radius\">Terrain size</label><select id=\"practice-radius\"><option value=\"20\">Small</option><option value=\"40\" selected>Medium</option><option value=\"70\">Large</option></select></div>"
    } else {
        "<p class=\"practice-readonly\">Only the room owner can edit.</p>"
    };
    let precise = owner
        .then(|| precise_placement_html(model, false))
        .unwrap_or_default();
    format!(
        "<section class=\"paper-card practice-editor\" aria-labelledby=\"practice-title\"><div class=\"map-heading\"><div><p class=\"eyebrow\">Practice setup</p><h2 id=\"practice-title\">Battlefield editor</h2></div><span class=\"room-kind\">Practice</span></div>{controls}<div class=\"practice-field\"><canvas id=\"practice-canvas\" width=\"770\" height=\"450\" aria-label=\"Practice battlefield setup\"></canvas></div><p id=\"practice-status\" class=\"practice-status\" role=\"status\" aria-live=\"polite\">{}</p>{precise}</section>",
        practice_setup_status(model.practice_setup.as_ref())
    )
}

fn precise_placement_html(model: &Model, open: bool) -> String {
    let soldier_options = model
        .practice_setup
        .as_ref()
        .into_iter()
        .flat_map(|setup| setup.players.iter())
        .flat_map(|placement| {
            let name = model
                .players
                .iter()
                .find(|player| player.id == placement.player_id.to_string())
                .map(|player| player.name.as_str())
                .unwrap_or("Player");
            placement
                .soldiers
                .iter()
                .enumerate()
                .map(move |(index, _)| {
                    format!(
                        "<option value=\"{}:{}\">{} · soldier {}</option>",
                        placement.player_id,
                        index,
                        escape(name),
                        index + 1
                    )
                })
        })
        .collect::<String>();
    let circles = model
        .practice_setup
        .as_ref()
        .into_iter()
        .flat_map(|setup| setup.terrain.iter().enumerate())
        .map(|(index, circle)| {
            format!(
                "<li>Circle {} · ({:.0}, {:.0}) · r{:.0}<button type=\"button\" class=\"practice-remove-circle text-button\" data-index=\"{index}\">Remove</button></li>",
                index + 1,
                circle.x,
                circle.y,
                circle.radius,
            )
        })
        .collect::<String>();
    let open = open.then_some(" open").unwrap_or("");
    format!(
        "<details class=\"practice-precise\"{open}><summary>Precise placement</summary><form id=\"practice-soldier-form\"><label for=\"practice-soldier\">Soldier</label><select id=\"practice-soldier\">{soldier_options}</select><label for=\"practice-x\">X</label><input id=\"practice-x\" type=\"number\" min=\"7\" max=\"762\" step=\"1\" required><label for=\"practice-y\">Y</label><input id=\"practice-y\" type=\"number\" min=\"7\" max=\"442\" step=\"1\" required><button class=\"secondary\" type=\"submit\">Apply soldier position</button></form><form id=\"practice-terrain-form\"><label for=\"practice-terrain-x\">Terrain X</label><input id=\"practice-terrain-x\" type=\"number\" min=\"20\" max=\"750\" step=\"1\" required><label for=\"practice-terrain-y\">Terrain Y</label><input id=\"practice-terrain-y\" type=\"number\" min=\"20\" max=\"430\" step=\"1\" required><label for=\"practice-terrain-radius\">Size</label><select id=\"practice-terrain-radius\"><option value=\"20\">Small</option><option value=\"40\">Medium</option><option value=\"70\">Large</option></select><button class=\"secondary\" type=\"submit\">Add terrain</button></form><ul class=\"practice-circle-list\">{circles}</ul></details>"
    )
}

fn room_team_rosters_html(model: &Model) -> String {
    [1, 2]
        .into_iter()
        .map(|team| {
            let items = model
                .players
                .iter()
                .filter(|player| player.team == team || (team == 2 && player.team != 1))
                .map(|player| room_player_item_html(model, player))
                .collect::<String>();
            let empty = items
                .is_empty()
                .then_some("<p class=\"empty-team\">Waiting for players</p>")
                .unwrap_or("");
            format!(
                "<section class=\"team-roster team-roster-{team}\" data-team=\"{team}\" aria-labelledby=\"team-{team}-title\"><h3 id=\"team-{team}-title\">{}</h3><ul data-team=\"{team}\">{items}</ul>{empty}<button id=\"team-{team}-target\" type=\"button\" class=\"team-drop-target secondary\" data-team=\"{team}\" aria-disabled=\"true\">Move selected to {}</button></section>",
                team_name(team),
                team_name(team),
            )
        })
        .collect()
}

fn can_move_player(model: &Model, player: &crate::state::PlayerSummary) -> bool {
    model.local_owner() || model.player_id.as_deref() == Some(player.id.as_str())
}

fn room_player_item_html(model: &Model, player: &crate::state::PlayerSummary) -> String {
    let owner_controls = model.local_owner();
    let local = model.player_id.as_deref() == Some(player.id.as_str());
    let movable = can_move_player(model, player);
    let draggable = movable.then_some(" draggable=\"true\"").unwrap_or("");
    let move_control = movable
        .then(|| {
            format!(
                "<button type=\"button\" class=\"select-player text-button\" data-player-id=\"{}\" data-player-team=\"{}\" aria-label=\"Select {} to move teams\" aria-pressed=\"false\">Move</button>",
                attr(&player.id),
                player.team,
                attr(&player.name),
            )
        })
        .unwrap_or_default();
    let disabled = (!(local || (owner_controls && player.is_bot)))
        .then_some(" disabled")
        .unwrap_or("");
    let remove = (owner_controls && !local)
        .then(|| {
            format!(
                "<button type=\"button\" class=\"remove-player\" data-player-id=\"{}\" data-is-bot=\"{}\" aria-label=\"Remove {} from room\">x</button>",
                attr(&player.id),
                player.is_bot,
                attr(&player.name),
            )
        })
        .unwrap_or_default();
    format!(
        "<li class=\"player-slot\" data-player-id=\"{}\" data-player-team=\"{}\"{draggable}><strong>{}</strong><div class=\"player-controls\">{move_control}<label><span class=\"sr-only\">{} soldiers</span><select class=\"player-soldiers\" data-player-id=\"{}\"{}>{}</select></label>{remove}</div></li>",
        attr(&player.id),
        player.team,
        escape(&player.name),
        escape(&player.name),
        attr(&player.id),
        disabled,
        (1..=4)
            .map(|count| format!(
                "<option value=\"{count}\"{}>{count}</option>",
                (player.soldiers == count)
                    .then_some(" selected")
                    .unwrap_or("")
            ))
            .collect::<String>(),
    )
}

fn game_html(model: &Model) -> String {
    let local_turn = local_turn(model);
    let disabled = (!local_turn).then_some(" disabled").unwrap_or("");
    let second_order = model.game_mode == Some(GameMode::SecondOrder);
    let angle_hidden = (!second_order).then_some(" hidden").unwrap_or("");
    let angle_disabled = (!second_order).then_some(" disabled").unwrap_or(disabled);
    let timer = timer_view(model);
    let fire_label = fire_label(model, local_turn);
    let finished_hidden = (model.room_phase != Some(Phase::Finished))
        .then_some(" hidden")
        .unwrap_or("");
    let return_disabled = (!model.local_owner()).then_some(" disabled").unwrap_or("");
    let finished_hint = if model.local_owner() {
        "Return everyone to the staging area."
    } else {
        "Waiting for the room owner to return to the staging area."
    };
    format!(
        "<section class=\"game-shell reveal\" aria-label=\"Active match\"><div class=\"war-room\"><section class=\"map-panel field-map\" aria-labelledby=\"battlefield-label\"><div class=\"map-heading\"><div><p class=\"eyebrow\">Coordinate field / 01</p><h2 id=\"battlefield-label\">Battlefield</h2></div><button id=\"leave-room\" class=\"text-button\">Retreat</button></div><div class=\"battlefield\"><canvas id=\"game-canvas\" width=\"770\" height=\"450\" aria-label=\"Graphwar battlefield\" aria-describedby=\"battlefield-summary\"></canvas>{}<div class=\"preview-key\"><i></i> Provisional</div><div class=\"axis-label x-label\">x</div><div class=\"axis-label y-label\">y</div></div><p id=\"shot-status\" class=\"shot-status\" role=\"status\" aria-live=\"polite\">{}</p><p id=\"battlefield-summary\" class=\"sr-only\">{}</p></section><aside class=\"command-stack\" aria-label=\"Command stack\"><section class=\"paper-card function-panel\" aria-labelledby=\"function-panel-title\"><h2 id=\"function-panel-title\">Function</h2><form id=\"fire-form\" class=\"fire-console\"><div class=\"equation-field\"><label for=\"function-input\">Function</label><div><span aria-hidden=\"true\">y =</span><input id=\"function-input\" spellcheck=\"false\" autocomplete=\"off\" maxlength=\"256\" required value=\"{}\" aria-describedby=\"function-hint function-error\"{disabled}></div><small id=\"function-hint\">Plain: sin, atan2, min, log. LaTeX paste supported, e.g. \\frac{{\\sin(x)}}{{\\sqrt{{2}}}}.</small><p id=\"function-error\" class=\"function-error\" aria-live=\"polite\"></p></div><div class=\"angle-field\"{angle_hidden}><div class=\"angle-label\"><label for=\"angle-input\">Launch angle</label><output id=\"angle-output\" for=\"angle-input\">{:.1}°</output></div><input id=\"angle-input\" type=\"range\" min=\"-90\" max=\"90\" value=\"{:.1}\" step=\"0.1\" aria-describedby=\"angle-hint angle-output\"{angle_disabled}><small id=\"angle-hint\">Focus the slider, then use Arrow Up/Down.</small></div><button class=\"fire-button\" type=\"submit\" style=\"--turn-progress: {}%;\" aria-describedby=\"turn-timer\"{disabled}><span class=\"fire-button-label\">{}</span><span id=\"turn-timer\" class=\"fire-button-timer\" role=\"timer\">{}</span><small>Enter ↵</small></button></form><div class=\"finished-actions\"{finished_hidden}><button id=\"return-to-lobby\" class=\"primary wide\" type=\"button\"{return_disabled}>Return to lobby</button><p>{}</p></div></section>{}</aside></div></section>",
        soldier_name_labels_html(model),
        escape(model.shot_status.as_deref().unwrap_or("")),
        escape(&battlefield_summary(model)),
        escape(if model.draft_function.is_empty() {
            "sin(x)"
        } else {
            &model.draft_function
        }),
        model.aim_angle_deg,
        model.aim_angle_deg,
        timer.progress,
        escape(&fire_label),
        escape(&timer.text),
        escape(finished_hint),
        chat_html(model, "game-chat")
    )
}

// ponytail: mirror the fixed server turn duration until protocol snapshots expose it.
const TURN_DURATION_SECONDS: i64 = 60;

struct TimerView {
    text: String,
    progress: u8,
}

fn timer_view(model: &Model) -> TimerView {
    match model.room_phase {
        Some(Phase::Resolving) => TimerView {
            text: "Resolving shot".into(),
            progress: 0,
        },
        Some(Phase::Finished) => TimerView {
            text: "Match finished".into(),
            progress: 0,
        },
        _ => model.turn_deadline_at.map_or_else(
            || TimerView {
                text: "Waiting".into(),
                progress: 0,
            },
            |deadline| {
                let remaining = deadline
                    .saturating_sub(unix_time())
                    .clamp(0, TURN_DURATION_SECONDS);
                TimerView {
                    text: format!("{remaining}s"),
                    progress: ((remaining * 100) / TURN_DURATION_SECONDS) as u8,
                }
            },
        ),
    }
}

fn fire_label(model: &Model, local_turn: bool) -> String {
    match model.room_phase {
        Some(Phase::Resolving) => "Resolving".into(),
        Some(Phase::Finished) => "Finished".into(),
        _ if local_turn => "Fire".into(),
        _ => "Waiting".into(),
    }
}

fn set_turn_progress(element: &web_sys::Element, progress: u8) -> Result<(), JsValue> {
    element.set_attribute("style", &format!("--turn-progress: {progress}%;"))
}

fn update_timer(app: &SharedApp) {
    let should_rerender = {
        let mut app_ref = app.borrow_mut();
        let deadline = app_ref.model.turn_deadline_at;
        let expired = app_ref.model.screen == Screen::Game
            && app_ref.model.room_phase == Some(Phase::Planning)
            && deadline.is_some_and(|deadline| deadline <= unix_time());
        if expired && app_ref.expired_deadline != deadline {
            app_ref.expired_deadline = deadline;
            true
        } else {
            if !expired {
                app_ref.expired_deadline = None;
            }
            false
        }
    };
    if should_rerender {
        refresh_game(app);
        return;
    }
    let (document, timer) = {
        let app_ref = app.borrow();
        if app_ref.model.screen != Screen::Game {
            return;
        }
        (app_ref.document.clone(), timer_view(&app_ref.model))
    };
    if let Some(timer_element) = document.get_element_by_id("turn-timer") {
        timer_element.set_text_content(Some(&timer.text));
    }
    if let Some(fire_button) = document.query_selector(".fire-button").ok().flatten() {
        if let Err(error) = set_turn_progress(&fire_button, timer.progress) {
            log_error(&format!("timer progress update failed: {error:?}"));
        }
    }
}

fn refresh_soldier_name_labels(document: &Document, model: &Model) -> Result<(), JsValue> {
    game_element(document, ".soldier-name-labels")?
        .set_inner_html(&soldier_name_label_items_html(model));
    Ok(())
}

fn soldier_name_labels_html(model: &Model) -> String {
    format!(
        "<div class=\"soldier-name-labels\" aria-hidden=\"true\">{}</div>",
        soldier_name_label_items_html(model)
    )
}

fn soldier_name_label_items_html(model: &Model) -> String {
    let turn_player_id = matches!(model.room_phase, Some(Phase::Planning | Phase::Resolving))
        .then(|| model.turn_player_id.as_deref())
        .flatten();
    model
        .soldiers
        .iter()
        .filter(|soldier| soldier.alive)
        .filter_map(|soldier| {
            let name = model
                .players
                .iter()
                .find(|player| player.id == soldier.player_id)?
                .name
                .as_str();
            let active = soldier.active && Some(soldier.player_id.as_str()) == turn_player_id;
            let class = active
                .then_some("soldier-name-label is-active")
                .unwrap_or("soldier-name-label");
            let x = (soldier.x * 100.0 / LOGICAL_WIDTH).clamp(0.0, 100.0);
            let y = (soldier.y * 100.0 / LOGICAL_HEIGHT).clamp(0.0, 100.0);
            Some(format!(
                "<span class=\"{class}\" data-player-id=\"{}\" data-soldier-index=\"{}\" style=\"--soldier-x: {x:.3}%; --soldier-y: {y:.3}%;\">{}</span>",
                attr(&soldier.player_id),
                soldier.index,
                escape(name),
            ))
        })
        .collect()
}

fn local_turn(model: &Model) -> bool {
    model.room_phase == Some(graphwar_protocol::Phase::Planning)
        && model
            .turn_deadline_at
            .is_some_and(|deadline| deadline > unix_time())
        && model.player_id == model.turn_player_id
}

fn unix_time() -> i64 {
    (js_sys::Date::now() / 1_000.0) as i64
}

fn bind_viewport_events(app: &SharedApp) -> Result<(), JsValue> {
    let window = app.borrow().window.clone();
    let redraw_app = Rc::clone(app);
    let handler = Closure::<dyn FnMut(Event)>::new(move |_| {
        let result = match (
            redraw_app.borrow().model.screen.clone(),
            redraw_app.borrow().model.room_kind,
        ) {
            (Screen::Game, _) => render_canvas(&redraw_app),
            (Screen::Room, Some(RoomKind::Practice)) => render_practice_canvas(&redraw_app),
            _ => Ok(()),
        };
        if let Err(error) = result {
            log_error(&format!("viewport render failed: {error:?}"));
        }
    });
    window.add_event_listener_with_callback("resize", handler.as_ref().unchecked_ref())?;
    app.borrow_mut().viewport_handler = Some(handler);
    Ok(())
}

fn team_name(team: u8) -> &'static str {
    if team == 1 { "Team One" } else { "Team Two" }
}

fn battlefield_summary(model: &Model) -> String {
    let team_one = model
        .soldiers
        .iter()
        .filter(|soldier| soldier.team == 1 && soldier.alive)
        .count();
    let team_two = model
        .soldiers
        .iter()
        .filter(|soldier| soldier.team == 2 && soldier.alive)
        .count();
    let active = model
        .soldiers
        .iter()
        .find(|soldier| soldier.active && soldier.alive);
    let active = active.map_or_else(
        || "No active soldier.".into(),
        |soldier| {
            let name = model
                .players
                .iter()
                .find(|player| player.id == soldier.player_id)
                .map(|player| player.name.as_str())
                .unwrap_or("Player");
            format!(
                "{} active for {} at ({:.0}, {:.0}).",
                name,
                team_name(soldier.team),
                soldier.x,
                soldier.y
            )
        },
    );
    let outcome = model
        .shot_status
        .as_deref()
        .map_or_else(String::new, |status| format!(" {status}."));
    format!(
        "Battlefield: Team One has {team_one} living soldier(s); Team Two has {team_two}. {active}{outcome}"
    )
}

fn chat_html(model: &Model, class_name: &str) -> String {
    format!(
        "<section class=\"paper-card chat-panel {class_name} field-log\" aria-labelledby=\"chat-title\"><h2 id=\"chat-title\">Room chat</h2><ul>{}</ul><form id=\"chat-form\" class=\"inline-form\"><label class=\"sr-only\" for=\"chat-input\">Message</label><input id=\"chat-input\" maxlength=\"500\" autocomplete=\"off\" required placeholder=\"Message the room\"><button class=\"secondary\" type=\"submit\">Send</button></form></section>",
        chat_messages_html(model)
    )
}

fn chat_messages_html(model: &Model) -> String {
    let mut entries = model
        .chat
        .iter()
        .map(FeedEntry::Chat)
        .chain(model.shot_history.iter().map(FeedEntry::Shot))
        .collect::<Vec<_>>();
    entries.sort_by_key(FeedEntry::sequence);
    entries.dedup_by_key(|entry| entry.sequence());
    let start = entries.len().saturating_sub(40);
    entries[start..]
        .iter()
        .map(|entry| match entry {
            FeedEntry::Chat(message) => format!(
                "<li class=\"chat-message\" data-sequence=\"{}\"><strong>{}:</strong> <span>{}</span></li>",
                message.sequence,
                escape(&message.display_name),
                escape(&message.text)
            ),
            FeedEntry::Shot(entry) => {
                let angle = (model.game_mode == Some(GameMode::SecondOrder))
                    .then(|| format!("<span>{:.1}°</span>", entry.angle_deg))
                    .unwrap_or_default();
                format!(
                    "<li class=\"shot-entry\" data-sequence=\"{}\"><strong><span class=\"team team-{}\" aria-hidden=\"true\"></span>{}</strong><code>{}</code>{}</li>",
                    entry.sequence,
                    entry.team,
                    escape(&entry.display_name),
                    escape(&entry.function),
                    angle,
                )
            }
        })
        .collect()
}

fn notices_html(model: &Model) -> String {
    format!(
        "<ul class=\"app-notices notices\" aria-label=\"Recent notices\">{}</ul>",
        notice_items_html(model)
    )
}

fn notice_items_html(model: &Model) -> String {
    model
        .notices
        .iter()
        .rev()
        .take(3)
        .map(|notice| format!("<li>{}</li>", escape(notice)))
        .collect()
}

fn bind_events(app: &SharedApp) -> Result<(), JsValue> {
    let document = app.borrow().document.clone();
    if let Some(button) = document.get_element_by_id("reconnect-now") {
        let app = Rc::clone(app);
        bind_click(&app.clone(), &button, move || {
            app.borrow_mut().reconnect_attempt = 0;
            if let Err(error) = connect(&app) {
                notice(&app, format!("connection failed: {error:?}"));
            }
        });
    }
    if let Some(button) = document.get_element_by_id("logout") {
        let app = Rc::clone(app);
        bind_click(&app.clone(), &button, move || logout(&app));
    }
    if let Some(form) = document.get_element_by_id("login-form") {
        let app = Rc::clone(app);
        bind_submit(&app.clone(), form.unchecked_into(), move |form| {
            let Some(email) = input_value(&form, "login-email") else {
                return;
            };
            let Some(password) = password_value(&form, "login-password") else {
                return;
            };
            login(&app, LoginRequest { email, password });
        });
    }
    if let Some(form) = document.get_element_by_id("register-form") {
        let app = Rc::clone(app);
        bind_submit(&app.clone(), form.unchecked_into(), move |form| {
            let Some(display_name) = input_value(&form, "register-name") else {
                return;
            };
            let Some(email) = input_value(&form, "register-email") else {
                return;
            };
            let Some(password) = password_value(&form, "register-password") else {
                return;
            };
            register(
                &app,
                RegisterRequest {
                    email,
                    display_name,
                    password,
                },
            );
        });
    }
    if let Some(button) = document.get_element_by_id("create-room-open")
        && let Some(dialog) = document
            .get_element_by_id("create-room-dialog")
            .and_then(|element| element.dyn_into::<HtmlDialogElement>().ok())
    {
        let dialog_for_open = dialog.clone();
        let document_for_open = document.clone();
        bind_click(app, &button, move || {
            if dialog_for_open.show_modal().is_ok()
                && let Some(input) = document_for_open
                    .get_element_by_id("room-name")
                    .and_then(|element| element.dyn_into::<HtmlInputElement>().ok())
            {
                let _ = input.focus();
            }
        });
        let document_for_close = document.clone();
        bind_event(app, &dialog, "close", move |_| {
            if let Some(button) = document_for_close
                .get_element_by_id("create-room-open")
                .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
            {
                let _ = button.focus();
            }
        })?;
    }
    if let Some(button) = document.get_element_by_id("create-room-cancel")
        && let Some(dialog) = document
            .get_element_by_id("create-room-dialog")
            .and_then(|element| element.dyn_into::<HtmlDialogElement>().ok())
    {
        bind_click(app, &button, move || dialog.close());
    }
    if let Some(select) = document
        .get_element_by_id("room-visibility")
        .and_then(|element| element.dyn_into::<HtmlSelectElement>().ok())
    {
        let bound_select = select.clone();
        let visibility_document = document.clone();
        bind_event(app, &select, "change", move |_| {
            set_create_room_password_visibility(
                &visibility_document,
                bound_select.value() == "private",
            );
        })?;
    }
    if let Some(form) = document.get_element_by_id("create-room-form") {
        let app = Rc::clone(app);
        bind_submit(&app.clone(), form.unchecked_into(), move |form| {
            if let Some(name) = input_value(&form, "room-name") {
                let visibility = form
                    .query_selector("#room-visibility")
                    .ok()
                    .flatten()
                    .and_then(|input| input.dyn_into::<HtmlSelectElement>().ok())
                    .is_some_and(|input| input.value() == "private")
                    .then_some(RoomVisibility::Private)
                    .unwrap_or(RoomVisibility::Public);
                let kind = form
                    .query_selector("#room-kind")
                    .ok()
                    .flatten()
                    .and_then(|input| input.dyn_into::<HtmlSelectElement>().ok())
                    .is_some_and(|input| input.value() == "practice")
                    .then_some(RoomKind::Practice)
                    .unwrap_or(RoomKind::Standard);
                let password = if visibility == RoomVisibility::Private {
                    let Some(password) = password_value(&form, "room-password") else {
                        notice(&app, "Private room password is required".into());
                        return;
                    };
                    Some(password)
                } else {
                    None
                };
                send(
                    &app,
                    ClientMessage::CreateRoom {
                        name,
                        visibility,
                        kind,
                        password,
                    },
                );
            }
        });
    }
    bind_lobby_room_events(app, &document)?;
    if let Some(button) = document.get_element_by_id("leave-room") {
        let app = Rc::clone(app);
        bind_click(&app.clone(), &button, move || {
            let (active, window) = {
                let app_ref = app.borrow();
                (
                    matches!(
                        app_ref.model.room_phase,
                        Some(Phase::Planning | Phase::Resolving)
                    ),
                    app_ref.window.clone(),
                )
            };
            if !active
                || window
                    .confirm_with_message("Retreat and forfeit this match?")
                    .unwrap_or(false)
            {
                send(&app, ClientMessage::LeaveRoom);
            }
        });
    }
    if let Some(button) = document.get_element_by_id("ready-button") {
        let app = Rc::clone(app);
        bind_click(&app.clone(), &button, move || {
            let ready = !app.borrow().model.local_ready();
            send(&app, ClientMessage::SetReady { ready });
        });
    }
    let mode_inputs = document.query_selector_all("input[name=game-mode]")?;
    for index in 0..mode_inputs.length() {
        let Some(element) = mode_inputs.item(index) else {
            continue;
        };
        let element = element.unchecked_into::<HtmlInputElement>();
        let app = Rc::clone(app);
        bind_change(&app.clone(), &element, move |input| {
            let mode = match input.value().as_str() {
                "first_order" => GameMode::FirstOrder,
                "second_order" => GameMode::SecondOrder,
                _ => GameMode::Function,
            };
            send(&app, ClientMessage::SetMode { mode });
        })?;
    }
    if let Some(button) = document.get_element_by_id("add-bot") {
        let app = Rc::clone(app);
        bind_click(&app.clone(), &button, move || {
            send(&app, ClientMessage::AddBot { level: 4 });
        });
    }
    if let Some(button) = document.get_element_by_id("start-game") {
        let app = Rc::clone(app);
        bind_click(&app.clone(), &button, move || {
            send(&app, ClientMessage::StartGame);
        });
    }
    bind_room_roster_events(app, &document)?;
    if app.borrow().model.room_kind == Some(RoomKind::Practice) {
        bind_practice_events(app, &document)?;
        render_practice_canvas(app)?;
    }
    if let Some(input) = document.get_element_by_id("angle-input") {
        let input = input.unchecked_into::<HtmlInputElement>();
        let listener_input = input.clone();
        let angle_document = document.clone();
        let app = Rc::clone(app);
        let listener_app = Rc::clone(&app);
        let closure = Closure::<dyn FnMut(Event)>::new(move |_| {
            let value = listener_input.value_as_number().clamp(-90.0, 90.0);
            listener_app.borrow_mut().model.aim_angle_deg = value;
            if let Some(output) = angle_document.get_element_by_id("angle-output") {
                output.set_text_content(Some(&format!("{value:.1}°")));
            }
            if let Some(function) = angle_document
                .get_element_by_id("function-input")
                .and_then(|input| input.dyn_into::<HtmlInputElement>().ok())
            {
                update_preview(&listener_app, &function);
            }
        });
        let target = input.clone().unchecked_into::<web_sys::EventTarget>();
        target.add_event_listener_with_callback("input", closure.as_ref().unchecked_ref())?;
        retain_event_handler(&app, target, "input", closure);
    }
    if let Some(button) = document.get_element_by_id("return-to-lobby") {
        let app = Rc::clone(app);
        bind_click(&app.clone(), &button, move || {
            send(&app, ClientMessage::ReturnToLobby);
        });
    }
    if let Some(form) = document.get_element_by_id("fire-form") {
        let app = Rc::clone(app);
        bind_submit(&app.clone(), form.unchecked_into(), move |form| {
            let Some(function) = input_value(&form, "function-input") else {
                return;
            };
            let angle_deg = input_value(&form, "angle-input")
                .and_then(|value| value.parse::<f64>().ok())
                .unwrap_or_default()
                .clamp(-90.0, 90.0);
            {
                let mut app_ref = app.borrow_mut();
                app_ref.model.draft_function.clone_from(&function);
                app_ref.model.aim_angle_deg = angle_deg;
            }
            send(
                &app,
                ClientMessage::FireFunction {
                    function,
                    angle_deg,
                },
            );
        });
    }
    if let Some(input) = document.get_element_by_id("function-input") {
        let input = input.unchecked_into::<HtmlInputElement>();
        let preview_app = Rc::clone(app);
        let listener_input = input.clone();
        let closure = Closure::<dyn FnMut(Event)>::new(move |_| {
            update_preview(&preview_app, &listener_input);
        });
        let target = input.clone().unchecked_into::<web_sys::EventTarget>();
        target.add_event_listener_with_callback("input", closure.as_ref().unchecked_ref())?;
        retain_event_handler(&app, target, "input", closure);
        if app.borrow().model.preview_path.is_empty() && !input.disabled() {
            update_preview(app, &input);
        }
    }
    if let Some(form) = document.get_element_by_id("chat-form") {
        let app = Rc::clone(app);
        bind_submit(&app.clone(), form.unchecked_into(), move |form| {
            let Some(text) = input_value(&form, "chat-input") else {
                return;
            };
            send(&app, ClientMessage::Chat { text });
            if let Some(input) = form
                .query_selector("#chat-input")
                .ok()
                .flatten()
                .and_then(|input| input.dyn_into::<HtmlInputElement>().ok())
            {
                input.set_value("");
            }
        });
    }
    Ok(())
}

fn bind_lobby_room_events(app: &SharedApp, document: &Document) -> Result<(), JsValue> {
    let room_buttons = document.query_selector_all(".join-room")?;
    for index in 0..room_buttons.length() {
        let Some(element) = room_buttons.item(index) else {
            continue;
        };
        let element = element.unchecked_into::<web_sys::Element>();
        let room_id = element.get_attribute("data-room-id").unwrap_or_default();
        let protected = element
            .get_attribute("data-room-protected")
            .is_some_and(|value| value == "true");
        let event_app = Rc::clone(app);
        bind_dynamic_click(app, &element, move || {
            let Ok(room_id) = Uuid::parse_str(&room_id) else {
                log_error("invalid room ID");
                return;
            };
            let invite = if protected {
                let window = event_app.borrow().window.clone();
                match window.prompt_with_message("Room password") {
                    Ok(Some(password)) if !password.is_empty() => Some(password),
                    Ok(Some(_)) => {
                        notice(&event_app, "Room password is required".into());
                        return;
                    }
                    Ok(None) => return,
                    Err(_) => return,
                }
            } else {
                None
            };
            send(&event_app, ClientMessage::JoinRoom { room_id, invite });
        });
    }
    Ok(())
}

fn bind_room_roster_events(app: &SharedApp, document: &Document) -> Result<(), JsValue> {
    if app.borrow_mut().selected_team_player.take().is_some()
        && let Some(status) = document.get_element_by_id("roster-move-status")
    {
        status.set_text_content(Some("Roster updated. Select a player to move teams."));
    }
    let inputs = document.query_selector_all(".player-soldiers")?;
    for index in 0..inputs.length() {
        let Some(element) = inputs.item(index) else {
            continue;
        };
        let element = element.unchecked_into::<HtmlSelectElement>();
        let player_id = element.get_attribute("data-player-id").unwrap_or_default();
        let event_app = Rc::clone(app);
        bind_dynamic_select_change(app, &element, move |input| {
            let Ok(player_id) = Uuid::parse_str(&player_id) else {
                return;
            };
            let soldiers = input.value().parse::<u8>().unwrap_or_default();
            send(
                &event_app,
                ClientMessage::SetSoldiers {
                    player_id,
                    soldiers,
                },
            );
        })?;
    }
    let select_buttons = document.query_selector_all(".select-player")?;
    for index in 0..select_buttons.length() {
        let Some(element) = select_buttons.item(index) else {
            continue;
        };
        let element = element.unchecked_into::<web_sys::Element>();
        let player_id = element.get_attribute("data-player-id").unwrap_or_default();
        let team = element
            .get_attribute("data-player-team")
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or_default();
        let name = element
            .get_attribute("aria-label")
            .unwrap_or_else(|| "Player".into())
            .trim_start_matches("Select ")
            .trim_end_matches(" to move teams")
            .to_owned();
        let event_app = Rc::clone(app);
        bind_dynamic_click(app, &element, move || {
            let Ok(player_id) = Uuid::parse_str(&player_id) else {
                return;
            };
            select_team_player(&event_app, player_id, team, &name);
        });
    }

    let targets = document.query_selector_all(".team-drop-target")?;
    for index in 0..targets.length() {
        let Some(element) = targets.item(index) else {
            continue;
        };
        let element = element.unchecked_into::<web_sys::Element>();
        let team = element
            .get_attribute("data-team")
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or_default();
        let event_app = Rc::clone(app);
        bind_dynamic_click(app, &element, move || send_selected_team(&event_app, team));
    }

    let cards = document.query_selector_all(".player-slot[draggable=\"true\"]")?;
    for index in 0..cards.length() {
        let Some(element) = cards.item(index) else {
            continue;
        };
        let element = element.unchecked_into::<web_sys::Element>();
        let player_id = element.get_attribute("data-player-id").unwrap_or_default();
        let team = element
            .get_attribute("data-player-team")
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or_default();
        let name = element
            .query_selector("strong")?
            .and_then(|node| node.text_content())
            .unwrap_or_else(|| "Player".into());
        let drag_app = Rc::clone(app);
        let drag_id = player_id.clone();
        let drag_name = name.clone();
        bind_dynamic_event(app, &element, "dragstart", move |event| {
            let Ok(player_id) = Uuid::parse_str(&drag_id) else {
                return;
            };
            if let Ok(Some(data)) = event
                .dyn_into::<DragEvent>()
                .map(|event| event.data_transfer())
            {
                let _ = data.set_data("text/plain", &drag_id);
                data.set_effect_allowed("move");
            }
            select_team_player(&drag_app, player_id, team, &drag_name);
            if let Some(card) = drag_app
                .borrow()
                .document
                .query_selector(&format!(
                    ".player-slot[data-player-id=\"{}\"]",
                    attr(&drag_id)
                ))
                .ok()
                .flatten()
            {
                let _ = card.set_attribute("data-dragging", "true");
            }
        });
        let end_app = Rc::clone(app);
        bind_dynamic_event(app, &element, "dragend", move |_| {
            clear_roster_drag_state(&end_app);
        });
    }

    let rosters = document.query_selector_all(".team-roster")?;
    for index in 0..rosters.length() {
        let Some(element) = rosters.item(index) else {
            continue;
        };
        let element = element.unchecked_into::<web_sys::Element>();
        let team = element
            .get_attribute("data-team")
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or_default();
        let over_element = element.clone();
        let over_app = Rc::clone(app);
        bind_dynamic_event(app, &element, "dragover", move |event| {
            let Some(selection) = over_app.borrow().selected_team_player else {
                return;
            };
            if selection.team == team {
                return;
            }
            event.prevent_default();
            let _ = over_element.set_attribute("data-drop-active", "true");
        });
        let leave_element = element.clone();
        bind_dynamic_event(app, &element, "dragleave", move |_| {
            let _ = leave_element.remove_attribute("data-drop-active");
        });
        let drop_element = element.clone();
        let drop_app = Rc::clone(app);
        bind_dynamic_event(app, &element, "drop", move |event| {
            event.prevent_default();
            let _ = drop_element.remove_attribute("data-drop-active");
            let player_id = event
                .dyn_into::<DragEvent>()
                .ok()
                .and_then(|event| event.data_transfer())
                .and_then(|data| data.get_data("text/plain").ok())
                .and_then(|value| Uuid::parse_str(&value).ok());
            if let Some(player_id) = player_id {
                send_team_change(&drop_app, player_id, team);
            } else {
                send_selected_team(&drop_app, team);
            }
        });
    }

    let buttons = document.query_selector_all(".remove-player")?;
    for index in 0..buttons.length() {
        let Some(element) = buttons.item(index) else {
            continue;
        };
        let element = element.unchecked_into::<web_sys::Element>();
        let player_id = element.get_attribute("data-player-id").unwrap_or_default();
        let is_bot = element.get_attribute("data-is-bot").as_deref() == Some("true");
        let event_app = Rc::clone(app);
        bind_dynamic_click(app, &element, move || {
            let Ok(player_id) = Uuid::parse_str(&player_id) else {
                return;
            };
            send(
                &event_app,
                if is_bot {
                    ClientMessage::RemoveBot { player_id }
                } else {
                    ClientMessage::KickPlayer { player_id }
                },
            );
        });
    }
    Ok(())
}

fn bind_practice_events(app: &SharedApp, document: &Document) -> Result<(), JsValue> {
    if !app.borrow().model.local_owner() {
        return Ok(());
    }
    let tools = document.query_selector_all(".practice-tool")?;
    for index in 0..tools.length() {
        let Some(element) = tools.item(index) else {
            continue;
        };
        let element = element.unchecked_into::<web_sys::Element>();
        let tool = element.get_attribute("data-tool").unwrap_or_default();
        let tool_app = Rc::clone(app);
        bind_dynamic_click(app, &element, move || {
            tool_app.borrow_mut().practice_tool = match tool.as_str() {
                "add" => PracticeTool::Add,
                "erase" => PracticeTool::Erase,
                _ => PracticeTool::Move,
            };
            update_practice_toolbar(&tool_app);
        });
    }
    if let Some(select) = document
        .get_element_by_id("practice-radius")
        .and_then(|element| element.dyn_into::<HtmlSelectElement>().ok())
    {
        select.set_value(&app.borrow().practice_radius.to_string());
        let radius_app = Rc::clone(app);
        bind_dynamic_select_change(app, &select, move |select| {
            radius_app.borrow_mut().practice_radius = select.value().parse().unwrap_or(40.0);
        })?;
    }
    if let Some(form) = document
        .get_element_by_id("practice-soldier-form")
        .and_then(|element| element.dyn_into::<HtmlFormElement>().ok())
    {
        let form_app = Rc::clone(app);
        bind_dynamic_submit(app, form, move |form| {
            let selected = form
                .query_selector("#practice-soldier")
                .ok()
                .flatten()
                .and_then(|element| element.dyn_into::<HtmlSelectElement>().ok())
                .map(|select| select.value());
            let Some((player, index)) = selected.as_deref().and_then(parse_practice_soldier) else {
                notice(&form_app, "Select a soldier".into());
                return;
            };
            let Some(x) = numeric_input(&form, "practice-x") else {
                notice(&form_app, "Enter a valid X coordinate".into());
                return;
            };
            let Some(y) = numeric_input(&form, "practice-y") else {
                notice(&form_app, "Enter a valid Y coordinate".into());
                return;
            };
            mutate_practice_setup(&form_app, move |setup| {
                let placement = setup
                    .players
                    .iter_mut()
                    .find(|placement| placement.player_id == player)
                    .ok_or("Soldier is no longer in the room")?;
                let point = placement
                    .soldiers
                    .get_mut(index)
                    .ok_or("Soldier is no longer in the room")?;
                *point = SetupPoint { x, y };
                Ok(())
            });
        });
    }
    if let Some(form) = document
        .get_element_by_id("practice-terrain-form")
        .and_then(|element| element.dyn_into::<HtmlFormElement>().ok())
    {
        let form_app = Rc::clone(app);
        bind_dynamic_submit(app, form, move |form| {
            let Some(x) = numeric_input(&form, "practice-terrain-x") else {
                notice(&form_app, "Enter a valid terrain X coordinate".into());
                return;
            };
            let Some(y) = numeric_input(&form, "practice-terrain-y") else {
                notice(&form_app, "Enter a valid terrain Y coordinate".into());
                return;
            };
            let radius = form
                .query_selector("#practice-terrain-radius")
                .ok()
                .flatten()
                .and_then(|element| element.dyn_into::<HtmlSelectElement>().ok())
                .and_then(|select| select.value().parse::<f64>().ok())
                .unwrap_or(40.0);
            mutate_practice_setup(&form_app, move |setup| {
                if setup.terrain.len() >= MAX_PRACTICE_TERRAIN_CIRCLES {
                    return Err("Terrain limit reached");
                }
                setup.terrain.push(TerrainCircle { x, y, radius });
                Ok(())
            });
        });
    }
    let removes = document.query_selector_all(".practice-remove-circle")?;
    for index in 0..removes.length() {
        let Some(element) = removes.item(index) else {
            continue;
        };
        let element = element.unchecked_into::<web_sys::Element>();
        let circle = element
            .get_attribute("data-index")
            .and_then(|index| index.parse::<usize>().ok());
        let remove_app = Rc::clone(app);
        bind_dynamic_click(app, &element, move || {
            let Some(circle) = circle else { return };
            mutate_practice_setup(&remove_app, move |setup| {
                if circle >= setup.terrain.len() {
                    return Err("Terrain circle is no longer present");
                }
                setup.terrain.remove(circle);
                Ok(())
            });
        });
    }
    let Some(canvas) = document
        .get_element_by_id("practice-canvas")
        .and_then(|element| element.dyn_into::<HtmlCanvasElement>().ok())
    else {
        return Ok(());
    };
    bind_practice_pointer(app, &canvas, "pointerdown", practice_pointer_down)?;
    bind_practice_pointer(app, &canvas, "pointermove", practice_pointer_move)?;
    bind_practice_pointer(app, &canvas, "pointerup", practice_pointer_up)?;
    bind_practice_pointer(app, &canvas, "pointercancel", practice_pointer_cancel)?;
    update_practice_toolbar(app);
    Ok(())
}

fn update_practice_toolbar(app: &SharedApp) {
    let (document, tool) = {
        let app = app.borrow();
        (app.document.clone(), app.practice_tool)
    };
    if let Ok(buttons) = document.query_selector_all(".practice-tool") {
        for index in 0..buttons.length() {
            let Some(button) = buttons
                .item(index)
                .and_then(|button| button.dyn_into::<web_sys::Element>().ok())
            else {
                continue;
            };
            let selected = matches!(
                (button.get_attribute("data-tool").as_deref(), tool),
                (Some("move"), PracticeTool::Move)
                    | (Some("add"), PracticeTool::Add)
                    | (Some("erase"), PracticeTool::Erase)
            );
            let _ = button.set_attribute("aria-pressed", if selected { "true" } else { "false" });
        }
    }
}

fn bind_practice_pointer(
    app: &SharedApp,
    canvas: &HtmlCanvasElement,
    event_name: &'static str,
    handler: fn(&SharedApp, HtmlCanvasElement, PointerEvent),
) -> Result<(), JsValue> {
    let event_app = Rc::clone(app);
    let bound_canvas = canvas.clone();
    bind_dynamic_event_target(app, canvas, event_name, move |event| {
        let Ok(event) = event.dyn_into::<PointerEvent>() else {
            return;
        };
        handler(&event_app, bound_canvas.clone(), event);
    })
}

fn practice_pointer_down(app: &SharedApp, canvas: HtmlCanvasElement, event: PointerEvent) {
    event.prevent_default();
    let Some((x, y)) = practice_pointer_point(app, &canvas, &event) else {
        return;
    };
    let tool = app.borrow().practice_tool;
    match tool {
        PracticeTool::Move => {
            let hit = practice_setup(app).and_then(|setup| practice_soldier_hit(&setup, x, y));
            if let Some((player_id, soldier_index)) = hit {
                let _ = canvas.set_pointer_capture(event.pointer_id());
                app.borrow_mut().practice_drag = Some(PracticeDrag {
                    pointer_id: event.pointer_id(),
                    player_id,
                    soldier_index,
                });
            } else {
                practice_status(app, "Select a soldier, then drag it.");
            }
        }
        PracticeTool::Add => {
            let radius = app.borrow().practice_radius;
            mutate_practice_setup(app, move |setup| {
                if setup.terrain.len() >= MAX_PRACTICE_TERRAIN_CIRCLES {
                    return Err("Terrain limit reached");
                }
                setup.terrain.push(TerrainCircle { x, y, radius });
                Ok(())
            });
        }
        PracticeTool::Erase => {
            mutate_practice_setup(app, move |setup| {
                let Some(index) = setup
                    .terrain
                    .iter()
                    .rposition(|circle| (circle.x - x).hypot(circle.y - y) <= circle.radius)
                else {
                    return Err("No terrain circle at that point");
                };
                setup.terrain.remove(index);
                Ok(())
            });
        }
    }
}

fn practice_pointer_move(shared: &SharedApp, canvas: HtmlCanvasElement, event: PointerEvent) {
    let drag = {
        let app = shared.borrow();
        app.practice_drag
            .as_ref()
            .filter(|drag| drag.pointer_id == event.pointer_id())
            .map(|drag| (drag.player_id, drag.soldier_index))
    };
    let Some((player_id, soldier_index)) = drag else {
        return;
    };
    let Some((x, y)) = practice_pointer_point(shared, &canvas, &event) else {
        return;
    };
    let mut app = shared.borrow_mut();
    if app.practice_draft.is_none() {
        app.practice_draft = Some(app.model.practice_setup.clone().unwrap_or_default());
    }
    let setup = app
        .practice_draft
        .as_mut()
        .expect("practice draft initialized");
    if let Some(point) = setup
        .players
        .iter_mut()
        .find(|placement| placement.player_id == player_id)
        .and_then(|placement| placement.soldiers.get_mut(soldier_index))
    {
        *point = SetupPoint { x, y };
    }
    drop(app);
    if let Err(error) = render_practice_canvas(shared) {
        log_error(&format!("practice canvas render failed: {error:?}"));
    }
}

fn practice_pointer_up(app: &SharedApp, canvas: HtmlCanvasElement, event: PointerEvent) {
    let drag = {
        let mut app = app.borrow_mut();
        app.practice_drag
            .take()
            .filter(|drag| drag.pointer_id == event.pointer_id())
    };
    let Some(drag) = drag else {
        return;
    };
    let _ = canvas.release_pointer_capture(event.pointer_id());
    if let Some((x, y)) = practice_pointer_point(app, &canvas, &event) {
        let player_id = drag.player_id;
        let soldier_index = drag.soldier_index;
        mutate_practice_setup(app, move |setup| {
            let point = setup
                .players
                .iter_mut()
                .find(|placement| placement.player_id == player_id)
                .and_then(|placement| placement.soldiers.get_mut(soldier_index))
                .ok_or("Soldier is no longer in the room")?;
            *point = SetupPoint { x, y };
            Ok(())
        });
    } else {
        app.borrow_mut().practice_draft = None;
        let _ = render_practice_canvas(app);
        practice_status(app, "Move cancelled.");
    }
}

fn practice_pointer_cancel(app: &SharedApp, canvas: HtmlCanvasElement, event: PointerEvent) {
    cancel_practice_drag(app, &canvas, event.pointer_id());
}

fn cancel_practice_drag(app: &SharedApp, canvas: &HtmlCanvasElement, pointer_id: i32) {
    let cancelled = {
        let mut app = app.borrow_mut();
        let cancelled = app
            .practice_drag
            .as_ref()
            .is_some_and(|drag| drag.pointer_id == pointer_id);
        if cancelled {
            app.practice_drag = None;
            app.practice_draft = None;
        }
        cancelled
    };
    if cancelled {
        let _ = canvas.release_pointer_capture(pointer_id);
        let _ = render_practice_canvas(app);
        practice_status(app, "Move cancelled.");
    }
}

fn practice_pointer_point(
    app: &SharedApp,
    canvas: &HtmlCanvasElement,
    event: &PointerEvent,
) -> Option<(f64, f64)> {
    let rect = canvas.get_bounding_client_rect();
    let viewport = Viewport::new(
        rect.width(),
        rect.height(),
        app.borrow().window.device_pixel_ratio(),
    );
    let (x, y) = viewport.css_to_logical(
        f64::from(event.client_x()) - rect.left(),
        f64::from(event.client_y()) - rect.top(),
    );
    (x.is_finite() && y.is_finite()).then_some((x, y))
}

fn practice_setup(app: &SharedApp) -> Option<PracticeSetup> {
    let app = app.borrow();
    app.practice_draft
        .clone()
        .or_else(|| app.model.practice_setup.clone())
}

fn practice_soldier_hit(setup: &PracticeSetup, x: f64, y: f64) -> Option<(Uuid, usize)> {
    setup.players.iter().find_map(|placement| {
        placement
            .soldiers
            .iter()
            .enumerate()
            .rev()
            .find(|(_, point)| (point.x - x).hypot(point.y - y) <= 14.0)
            .map(|(index, _)| (placement.player_id, index))
    })
}

fn mutate_practice_setup(
    app: &SharedApp,
    mutation: impl FnOnce(&mut PracticeSetup) -> Result<(), &'static str>,
) {
    let Some(mut setup) = practice_setup(app) else {
        practice_status(app, "Setup is not available yet.");
        return;
    };
    if let Err(message) = mutation(&mut setup).and_then(|_| validate_practice_setup_client(&setup))
    {
        app.borrow_mut().practice_draft = None;
        practice_status(app, message);
        let _ = render_practice_canvas(app);
        return;
    }
    app.borrow_mut().practice_draft = Some(setup.clone());
    if let Err(error) = render_practice_canvas(app) {
        log_error(&format!("practice canvas render failed: {error:?}"));
    }
    submit_practice_setup(app, setup);
}

fn submit_practice_setup(app: &SharedApp, setup: PracticeSetup) {
    let (room_id, base_revision, pending) = {
        let app_ref = app.borrow();
        (
            app_ref.model.room_id.clone(),
            app_ref.model.room_revision,
            app_ref.practice_save.is_some(),
        )
    };
    let (Some(room_id), Some(base_revision)) = (room_id, base_revision) else {
        practice_status(app, "Setup revision is unavailable.");
        return;
    };
    if pending {
        practice_status(app, "Setup queued…");
        return;
    }
    if send(
        app,
        ClientMessage::SetPracticeSetup {
            base_revision,
            setup: setup.clone(),
        },
    ) {
        app.borrow_mut().practice_save = Some(PracticeSave {
            room_id,
            base_revision,
            submitted: setup,
        });
        practice_status(app, "Saving setup…");
    } else {
        practice_status(app, "Setup kept locally; reconnect to retry.");
    }
}

fn validate_practice_setup_client(setup: &PracticeSetup) -> Result<(), &'static str> {
    if setup.terrain.len() > MAX_PRACTICE_TERRAIN_CIRCLES {
        return Err("Terrain limit reached");
    }
    if setup.terrain.iter().any(|circle| {
        !circle.x.is_finite()
            || !circle.y.is_finite()
            || !PRACTICE_TERRAIN_RADII.contains(&circle.radius)
            || circle.x - circle.radius < 0.0
            || circle.x + circle.radius > LOGICAL_WIDTH
            || circle.y - circle.radius < 0.0
            || circle.y + circle.radius > LOGICAL_HEIGHT
    }) {
        return Err("Keep each terrain circle fully inside the battlefield.");
    }
    let mut soldiers = Vec::new();
    for point in setup
        .players
        .iter()
        .flat_map(|placement| &placement.soldiers)
    {
        if !point.x.is_finite()
            || !point.y.is_finite()
            || point.x - SOLDIER_RADIUS < 0.0
            || point.x + SOLDIER_RADIUS >= LOGICAL_WIDTH
            || point.y - SOLDIER_RADIUS < 0.0
            || point.y + SOLDIER_RADIUS >= LOGICAL_HEIGHT
            || setup.terrain.iter().any(|circle| {
                (circle.x - point.x).hypot(circle.y - point.y) <= circle.radius + SOLDIER_RADIUS
            })
            || soldiers.iter().any(|other: &SetupPoint| {
                (other.x - point.x).abs() < 20.0 && (other.y - point.y).abs() < 20.0
            })
        {
            return Err("Move soldiers inside the field, away from terrain and each other.");
        }
        soldiers.push(point.clone());
    }
    Ok(())
}

fn practice_status(app: &SharedApp, message: &str) {
    if let Some(status) = app.borrow().document.get_element_by_id("practice-status") {
        status.set_text_content(Some(message));
    }
    announce(app, message);
}

fn parse_practice_soldier(value: &str) -> Option<(Uuid, usize)> {
    let (player, index) = value.split_once(':')?;
    Some((Uuid::parse_str(player).ok()?, index.parse().ok()?))
}

fn numeric_input(form: &HtmlFormElement, id: &str) -> Option<f64> {
    let value = form
        .query_selector(&format!("#{id}"))
        .ok()??
        .dyn_into::<HtmlInputElement>()
        .ok()?
        .value_as_number();
    value.is_finite().then_some(value)
}

fn bind_dynamic_event(
    app: &SharedApp,
    element: &web_sys::Element,
    event_name: &'static str,
    handler: impl FnMut(Event) + 'static,
) {
    let _ = bind_dynamic_event_target(app, element, event_name, handler);
}

fn select_team_player(app: &SharedApp, player_id: Uuid, team: u8, name: &str) {
    {
        let mut app_ref = app.borrow_mut();
        app_ref.selected_team_player = Some(TeamMoveSelection { player_id, team });
        let player_id = player_id.to_string();
        if let Ok(buttons) = app_ref.document.query_selector_all(".select-player") {
            for index in 0..buttons.length() {
                if let Some(button) = buttons
                    .item(index)
                    .and_then(|button| button.dyn_into::<web_sys::Element>().ok())
                {
                    let selected = button.get_attribute("data-player-id").as_deref()
                        == Some(player_id.as_str());
                    let _ = button
                        .set_attribute("aria-pressed", if selected { "true" } else { "false" });
                }
            }
        }
        if let Ok(cards) = app_ref.document.query_selector_all(".player-slot") {
            for index in 0..cards.length() {
                if let Some(card) = cards
                    .item(index)
                    .and_then(|card| card.dyn_into::<web_sys::Element>().ok())
                {
                    let selected =
                        card.get_attribute("data-player-id").as_deref() == Some(player_id.as_str());
                    if selected {
                        let _ = card.set_attribute("data-selected", "true");
                    } else {
                        let _ = card.remove_attribute("data-selected");
                    }
                }
            }
        }
        if let Ok(targets) = app_ref.document.query_selector_all(".team-drop-target") {
            for index in 0..targets.length() {
                if let Some(target) = targets
                    .item(index)
                    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                {
                    let target_team = target
                        .get_attribute("data-team")
                        .and_then(|value| value.parse::<u8>().ok());
                    let enabled = target_team.is_some_and(|target_team| target_team != team);
                    let _ = target
                        .set_attribute("aria-disabled", if enabled { "false" } else { "true" });
                }
            }
        }
        if let Some(status) = app_ref.document.get_element_by_id("roster-move-status") {
            status.set_text_content(Some(&format!("{name} selected. Choose a target team.")));
        }
    }
}

fn send_selected_team(app: &SharedApp, target_team: u8) {
    let selection = app.borrow().selected_team_player;
    let Some(selection) = selection else {
        announce(app, "Select a player before choosing a team");
        return;
    };
    send_team_change(app, selection.player_id, target_team);
}

fn send_team_change(app: &SharedApp, player_id: Uuid, target_team: u8) {
    let current_team = app
        .borrow()
        .model
        .players
        .iter()
        .find(|player| player.id == player_id.to_string())
        .map(|player| player.team);
    let Some(current_team) = current_team else {
        return;
    };
    if current_team == target_team {
        announce(app, "Player is already on that team");
        return;
    }
    app.borrow_mut().selected_team_player = None;
    send(
        app,
        ClientMessage::SetTeam {
            player_id,
            team: target_team,
        },
    );
}

fn clear_roster_drag_state(app: &SharedApp) {
    if let Ok(elements) = app
        .borrow()
        .document
        .query_selector_all(".player-slot[data-dragging], .team-roster[data-drop-active]")
    {
        for index in 0..elements.length() {
            if let Some(element) = elements
                .item(index)
                .and_then(|element| element.dyn_into::<web_sys::Element>().ok())
            {
                let _ = element.remove_attribute("data-dragging");
                let _ = element.remove_attribute("data-drop-active");
            }
        }
    }
}

fn update_preview(app: &SharedApp, function: &HtmlInputElement) {
    let function_text = function.value();
    let preview_model = {
        let mut app_ref = app.borrow_mut();
        app_ref.model.draft_function.clone_from(&function_text);
        app_ref.model.clone()
    };
    let preview = trace_preview(&preview_model, &function_text, preview_model.aim_angle_deg);
    let document = {
        let mut app_ref = app.borrow_mut();
        app_ref.model.preview_path = preview.clone().unwrap_or_default();
        app_ref.document.clone()
    };

    let error = preview.err();
    function.set_custom_validity(error.as_deref().unwrap_or(""));
    if let Some(message) = document.get_element_by_id("function-error") {
        message.set_text_content(error.as_deref());
    }
    if let Err(error) = render_canvas(app) {
        log_error(&format!("preview render failed: {error:?}"));
    }
}

fn start_shot_animation(app: &SharedApp, sequence: u64) {
    let reduced_motion = app
        .borrow()
        .window
        .match_media("(prefers-reduced-motion: reduce)")
        .ok()
        .flatten()
        .is_some_and(|query| query.matches());
    if reduced_motion {
        let mut app_ref = app.borrow_mut();
        app_ref.shot_animation = None;
        apply_pending_game(&mut app_ref.model);
        return;
    }
    let started_at = js_sys::Date::now();
    app.borrow_mut().shot_animation = Some(ShotAnimation {
        sequence,
        started_at,
    });
    schedule_shot_frame(app, sequence);
}

fn schedule_shot_frame(app: &SharedApp, sequence: u64) {
    let frame_app = Rc::clone(app);
    let callback = Closure::once_into_js(move |_: f64| {
        let app_ref = frame_app.borrow();
        let path_len = app_ref.model.authoritative_path.len();
        let Some(animation) = app_ref.shot_animation.as_ref() else {
            return;
        };
        if animation.sequence != sequence {
            return;
        }
        let complete =
            visible_points(path_len, js_sys::Date::now() - animation.started_at) == path_len;
        drop(app_ref);
        if complete {
            let mut app_ref = frame_app.borrow_mut();
            app_ref.shot_animation = None;
            apply_pending_game(&mut app_ref.model);
            drop(app_ref);
            refresh_game(&frame_app);
        } else {
            if let Err(error) = render_canvas(&frame_app) {
                log_error(&format!("shot render failed: {error:?}"));
            }
            schedule_shot_frame(&frame_app, sequence);
        }
    });
    let result = app
        .borrow()
        .window
        .request_animation_frame(callback.unchecked_ref());
    if let Err(error) = result {
        log_error(&format!("animation frame failed: {error:?}"));
        let mut app_ref = app.borrow_mut();
        app_ref.shot_animation = None;
        apply_pending_game(&mut app_ref.model);
        drop(app_ref);
        refresh_game(app);
    }
}

fn retain_event_handler(
    app: &SharedApp,
    target: web_sys::EventTarget,
    event_name: &'static str,
    closure: Closure<dyn FnMut(Event)>,
) {
    app.borrow_mut().event_handlers.push(EventHandler {
        target,
        event_name,
        closure,
    });
}

fn retain_dynamic_event_handler(
    app: &SharedApp,
    target: web_sys::EventTarget,
    event_name: &'static str,
    closure: Closure<dyn FnMut(Event)>,
) {
    app.borrow_mut().dynamic_event_handlers.push(EventHandler {
        target,
        event_name,
        closure,
    });
}

fn bind_event<T>(
    app: &SharedApp,
    target: &T,
    event_name: &'static str,
    handler: impl FnMut(Event) + 'static,
) -> Result<(), JsValue>
where
    T: Clone + JsCast,
{
    let target = target.clone().unchecked_into::<web_sys::EventTarget>();
    let closure = Closure::<dyn FnMut(Event)>::new(handler);
    target.add_event_listener_with_callback(event_name, closure.as_ref().unchecked_ref())?;
    retain_event_handler(app, target, event_name, closure);
    Ok(())
}

fn bind_dynamic_event_target<T>(
    app: &SharedApp,
    target: &T,
    event_name: &'static str,
    handler: impl FnMut(Event) + 'static,
) -> Result<(), JsValue>
where
    T: Clone + JsCast,
{
    let target = target.clone().unchecked_into::<web_sys::EventTarget>();
    let closure = Closure::<dyn FnMut(Event)>::new(handler);
    target.add_event_listener_with_callback(event_name, closure.as_ref().unchecked_ref())?;
    retain_dynamic_event_handler(app, target, event_name, closure);
    Ok(())
}

fn bind_dynamic_click(
    app: &SharedApp,
    element: &web_sys::Element,
    mut handler: impl FnMut() + 'static,
) {
    let _ = bind_dynamic_event_target(app, element, "click", move |_| handler());
}

fn bind_dynamic_select_change(
    app: &SharedApp,
    input: &HtmlSelectElement,
    mut handler: impl FnMut(HtmlSelectElement) + 'static,
) -> Result<(), JsValue> {
    let bound_input = input.clone();
    bind_dynamic_event_target(app, input, "change", move |_| handler(bound_input.clone()))
}

fn bind_dynamic_submit(
    app: &SharedApp,
    form: HtmlFormElement,
    mut handler: impl FnMut(HtmlFormElement) + 'static,
) {
    let bound_form = form.clone();
    let _ = bind_dynamic_event_target(app, &form, "submit", move |event| {
        event.prevent_default();
        handler(bound_form.clone());
    });
}

fn bind_submit(
    app: &SharedApp,
    form: HtmlFormElement,
    mut handler: impl FnMut(HtmlFormElement) + 'static,
) {
    let bound_form = form.clone();
    let _ = bind_event(app, &form, "submit", move |event| {
        event.prevent_default();
        handler(bound_form.clone());
    });
}

fn bind_click(app: &SharedApp, element: &web_sys::Element, mut handler: impl FnMut() + 'static) {
    let _ = bind_event(app, element, "click", move |_| handler());
}

fn bind_change(
    app: &SharedApp,
    input: &HtmlInputElement,
    mut handler: impl FnMut(HtmlInputElement) + 'static,
) -> Result<(), JsValue> {
    let bound_input = input.clone();
    bind_event(app, input, "change", move |_| handler(bound_input.clone()))
}

fn set_create_room_password_visibility(document: &Document, private: bool) {
    let Some(field) = document.get_element_by_id("room-password-field") else {
        return;
    };
    let Some(input) = document
        .get_element_by_id("room-password")
        .and_then(|element| element.dyn_into::<HtmlInputElement>().ok())
    else {
        return;
    };
    let _ = set_boolean_attribute(&field, "hidden", !private);
    input.set_disabled(!private);
    input.set_required(private);
    if !private {
        input.set_value("");
    }
}

fn input_value(form: &HtmlFormElement, id: &str) -> Option<String> {
    let input = form.query_selector(&format!("#{id}")).ok()??;
    let value = input.dyn_into::<HtmlInputElement>().ok()?.value();
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn password_value(form: &HtmlFormElement, id: &str) -> Option<String> {
    let input = form.query_selector(&format!("#{id}")).ok()??;
    let value = input.dyn_into::<HtmlInputElement>().ok()?.value();
    (!value.is_empty()).then(|| value.to_owned())
}

fn canvas_palette() -> CanvasPalette {
    CanvasPalette {
        background: "#f4ecd8",
        grid: "rgba(16, 37, 31, .12)",
        axis: "#10251f",
        terrain: "#244a3b",
        terrain_stroke: "#1c1f1b",
        path: "#a33a2b",
        preview: "#666756",
        team_one: "#d8a52b",
        team_two: "#a33a2b",
        dead: "#6d7168",
        soldier_stroke: "#10251f",
        hit: "#a33a2b",
    }
}

fn render_practice_canvas(app: &SharedApp) -> Result<(), JsValue> {
    let app = app.borrow();
    let canvas = app
        .document
        .get_element_by_id("practice-canvas")
        .ok_or("#practice-canvas missing")?
        .dyn_into::<HtmlCanvasElement>()?;
    let rect = canvas.get_bounding_client_rect();
    let viewport = Viewport::new(rect.width(), rect.height(), app.window.device_pixel_ratio());
    let (bitmap_width, bitmap_height) = viewport.bitmap_size();
    canvas.set_width(bitmap_width);
    canvas.set_height(bitmap_height);
    let context = canvas
        .get_context("2d")?
        .ok_or("2d context unavailable")?
        .dyn_into::<CanvasRenderingContext2d>()?;
    context.set_transform(
        bitmap_width as f64 / LOGICAL_WIDTH,
        0.0,
        0.0,
        bitmap_height as f64 / LOGICAL_HEIGHT,
        0.0,
        0.0,
    )?;
    let palette = canvas_palette();
    context.set_image_smoothing_enabled(false);
    context.set_fill_style_str(palette.background);
    context.fill_rect(0.0, 0.0, LOGICAL_WIDTH, LOGICAL_HEIGHT);
    draw_grid(&context, palette);
    let setup = app
        .practice_draft
        .as_ref()
        .or(app.model.practice_setup.as_ref());
    if let Some(setup) = setup {
        draw_practice_terrain(&context, &setup.terrain, palette);
        for placement in &setup.players {
            let team = app
                .model
                .players
                .iter()
                .find(|player| player.id == placement.player_id.to_string())
                .map(|player| player.team)
                .unwrap_or(1);
            for point in &placement.soldiers {
                draw_soldier(
                    &context,
                    point.x,
                    point.y,
                    team,
                    true,
                    false,
                    app.soldier_sprite_loaded
                        .then_some(app.soldier_sprite.as_ref())
                        .flatten(),
                    if team == 1 {
                        app.soldier_helmet_team_one_loaded
                            .then_some(app.soldier_helmet_team_one.as_ref())
                            .flatten()
                    } else {
                        app.soldier_helmet_team_two_loaded
                            .then_some(app.soldier_helmet_team_two.as_ref())
                            .flatten()
                    },
                    palette,
                );
            }
        }
    }
    Ok(())
}

fn draw_practice_terrain(
    context: &CanvasRenderingContext2d,
    circles: &[TerrainCircle],
    palette: CanvasPalette,
) {
    context.set_fill_style_str(palette.terrain);
    context.set_stroke_style_str(palette.terrain_stroke);
    context.set_line_width(2.0);
    for circle in circles {
        context.begin_path();
        let _ = context.arc(
            circle.x,
            circle.y,
            circle.radius,
            0.0,
            std::f64::consts::TAU,
        );
        context.fill();
        context.stroke();
    }
}

fn render_canvas(app: &SharedApp) -> Result<(), JsValue> {
    let app = app.borrow();
    let canvas = app
        .document
        .get_element_by_id("game-canvas")
        .ok_or("#game-canvas missing")?
        .dyn_into::<HtmlCanvasElement>()?;
    let rect = canvas.get_bounding_client_rect();
    let viewport = Viewport::new(rect.width(), rect.height(), app.window.device_pixel_ratio());
    let (bitmap_width, bitmap_height) = viewport.bitmap_size();
    canvas.set_width(bitmap_width);
    canvas.set_height(bitmap_height);
    let context = canvas
        .get_context("2d")?
        .ok_or("2d context unavailable")?
        .dyn_into::<CanvasRenderingContext2d>()?;
    context.set_transform(
        bitmap_width as f64 / LOGICAL_WIDTH,
        0.0,
        0.0,
        bitmap_height as f64 / LOGICAL_HEIGHT,
        0.0,
        0.0,
    )?;
    let palette = canvas_palette();
    context.set_image_smoothing_enabled(false);
    let soldier_sprite = app
        .soldier_sprite_loaded
        .then(|| app.soldier_sprite.as_ref())
        .flatten();
    let soldier_helmet_team_one = app
        .soldier_helmet_team_one_loaded
        .then(|| app.soldier_helmet_team_one.as_ref())
        .flatten();
    let soldier_helmet_team_two = app
        .soldier_helmet_team_two_loaded
        .then(|| app.soldier_helmet_team_two.as_ref())
        .flatten();
    context.set_fill_style_str(palette.background);
    context.fill_rect(0.0, 0.0, LOGICAL_WIDTH, LOGICAL_HEIGHT);
    draw_grid(&context, palette);
    draw_terrain(&context, &app.model, palette);
    let authoritative_len = app
        .shot_animation
        .as_ref()
        .filter(|animation| animation.sequence == app.model.shot_sequence)
        .map(|animation| {
            visible_points(
                app.model.authoritative_path.len(),
                js_sys::Date::now() - animation.started_at,
            )
        })
        .unwrap_or(app.model.authoritative_path.len());
    draw_path(
        &context,
        &app.model.authoritative_path[..authoritative_len],
        false,
        palette,
    )?;
    draw_path(&context, &app.model.preview_path, true, palette)?;
    for soldier in &app.model.soldiers {
        draw_soldier(
            &context,
            soldier.x,
            soldier.y,
            soldier.team,
            soldier.alive,
            soldier.active,
            soldier_sprite,
            if soldier.team == 1 {
                soldier_helmet_team_one
            } else {
                soldier_helmet_team_two
            },
            palette,
        );
    }
    if authoritative_len == app.model.authoritative_path.len() {
        draw_shot_effects(&context, &app.model, palette);
    }
    Ok(())
}

fn draw_shot_effects(context: &CanvasRenderingContext2d, model: &Model, palette: CanvasPalette) {
    context.save();
    context.set_stroke_style_str(palette.hit);
    context.set_line_width(3.0);
    for hit in &model.shot_hits {
        if let Some(soldier) = model
            .soldiers
            .iter()
            .find(|soldier| soldier.player_id == hit.player_id && soldier.index == hit.index)
        {
            context.begin_path();
            let _ = context.arc(soldier.x, soldier.y, 11.0, 0.0, std::f64::consts::TAU);
            context.stroke();
        }
    }
    if let Some(explosion) = &model.shot_explosion {
        context.begin_path();
        let _ = context.arc(
            explosion.x,
            explosion.y,
            explosion.radius,
            0.0,
            std::f64::consts::TAU,
        );
        context.stroke();
    }
    context.restore();
}

fn draw_grid(context: &CanvasRenderingContext2d, palette: CanvasPalette) {
    context.set_stroke_style_str(palette.grid);
    context.set_line_width(0.65);
    for x in (0..=770).step_by(35) {
        context.begin_path();
        context.move_to(x as f64, 0.0);
        context.line_to(x as f64, LOGICAL_HEIGHT);
        context.stroke();
    }
    for y in (0..=450).step_by(30) {
        context.begin_path();
        context.move_to(0.0, y as f64);
        context.line_to(LOGICAL_WIDTH, y as f64);
        context.stroke();
    }
    context.set_stroke_style_str(palette.axis);
    context.set_line_width(1.5);
    context.begin_path();
    context.move_to(0.0, 225.0);
    context.line_to(LOGICAL_WIDTH, 225.0);
    context.move_to(385.0, 0.0);
    context.line_to(385.0, LOGICAL_HEIGHT);
    context.stroke();
}

fn draw_terrain(context: &CanvasRenderingContext2d, model: &Model, palette: CanvasPalette) {
    context.set_fill_style_str(palette.terrain);
    context.set_stroke_style_str(palette.terrain_stroke);
    context.set_line_width(2.0);
    for terrain in model.terrain.iter().filter(|terrain| !terrain.cut) {
        context.begin_path();
        let _ = context.arc(
            terrain.x,
            terrain.y,
            terrain.radius,
            0.0,
            std::f64::consts::TAU,
        );
        context.fill();
        context.stroke();
    }
    for terrain in model.terrain.iter().filter(|terrain| terrain.cut) {
        context.begin_path();
        let _ = context.arc(
            terrain.x,
            terrain.y,
            terrain.radius,
            0.0,
            std::f64::consts::TAU,
        );
        context.set_fill_style_str(palette.background);
        context.fill();

        context.save();
        context.clip();
        draw_grid(context, palette);
        context.restore();
    }
}

fn draw_path(
    context: &CanvasRenderingContext2d,
    path: &[(f64, f64)],
    provisional: bool,
    palette: CanvasPalette,
) -> Result<(), JsValue> {
    let Some((start, rest)) = path.split_first() else {
        return Ok(());
    };
    context.begin_path();
    context.move_to(start.0, start.1);
    for point in rest {
        context.line_to(point.0, point.1);
    }
    context.set_stroke_style_str(if provisional {
        palette.preview
    } else {
        palette.path
    });
    context.set_line_width(if provisional { 1.5 } else { 2.5 });
    if provisional {
        context.set_line_dash(&js_sys::Array::of2(
            &JsValue::from_f64(5.0),
            &JsValue::from_f64(5.0),
        ))?;
    }
    context.stroke();
    if provisional {
        context.set_line_dash(&js_sys::Array::new())?;
    }
    Ok(())
}

fn draw_soldier(
    context: &CanvasRenderingContext2d,
    x: f64,
    y: f64,
    team: u8,
    alive: bool,
    active: bool,
    sprite: Option<&HtmlImageElement>,
    helmet: Option<&HtmlImageElement>,
    palette: CanvasPalette,
) {
    if alive && let Some(sprite) = sprite {
        if active {
            draw_soldier_fallback(context, x, y, team, alive, active, palette);
        }
        let drawn = if team == 2 {
            context.save();
            let result = context
                .translate(x, y)
                .and_then(|_| context.scale(-1.0, 1.0))
                .and_then(|_| draw_soldier_layers(context, sprite, helmet, -10.0, -10.0));
            context.restore();
            result.is_ok()
        } else {
            draw_soldier_layers(context, sprite, helmet, x - 10.0, y - 10.0).is_ok()
        };
        if drawn || active {
            return;
        }
    }
    draw_soldier_fallback(context, x, y, team, alive, active, palette);
}

fn draw_soldier_layers(
    context: &CanvasRenderingContext2d,
    sprite: &HtmlImageElement,
    helmet: Option<&HtmlImageElement>,
    x: f64,
    y: f64,
) -> Result<(), JsValue> {
    context.draw_image_with_html_image_element_and_dw_and_dh(sprite, x, y, 20.0, 20.0)?;
    if let Some(helmet) = helmet {
        let _ = context.draw_image_with_html_image_element_and_dw_and_dh(helmet, x, y, 20.0, 20.0);
    }
    Ok(())
}

fn draw_soldier_fallback(
    context: &CanvasRenderingContext2d,
    x: f64,
    y: f64,
    team: u8,
    alive: bool,
    active: bool,
    palette: CanvasPalette,
) {
    let color = if alive {
        if team % 2 == 0 {
            palette.team_two
        } else {
            palette.team_one
        }
    } else {
        palette.dead
    };
    let radius = if active && alive { 7.0 } else { 5.0 };
    context.begin_path();
    if team == 1 {
        let _ = context.arc(x, y, radius, 0.0, std::f64::consts::TAU);
    } else {
        context.rect(x - radius, y - radius, radius * 2.0, radius * 2.0);
    }
    context.set_fill_style_str(color);
    context.fill();
    context.set_stroke_style_str(palette.soldier_stroke);
    context.set_line_width(if active && alive { 2.5 } else { 1.5 });
    context.stroke();
    if !alive {
        context.begin_path();
        context.move_to(x - 7.0, y - 7.0);
        context.line_to(x + 7.0, y + 7.0);
        context.stroke();
    }
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn attr(value: &str) -> String {
    escape(value)
}

fn log_error(message: &str) {
    web_sys::console::error_1(&JsValue::from_str(message));
}
