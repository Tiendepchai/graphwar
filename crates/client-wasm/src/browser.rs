use std::{cell::RefCell, rc::Rc};

use gloo_net::http::Request;
use gloo_timers::callback::{Interval, Timeout};
use graphwar_protocol::{
    AccountResponse, ClientMessage, GameMode, LoginRequest, PROTOCOL_VERSION, RegisterRequest,
    RoomVisibility, ServerMessage,
};
use uuid::Uuid;
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::spawn_local;
use web_sys::{
    AbortController, CanvasRenderingContext2d, CloseEvent, Document, ErrorEvent, Event,
    HtmlCanvasElement, HtmlFormElement, HtmlInputElement, HtmlSelectElement, MessageEvent,
    RequestCredentials, WebSocket, Window,
};

const PRESERVED_INPUTS: &[&str] = &[
    "login-email",
    "login-password",
    "register-name",
    "register-email",
    "register-password",
    "room-name",
    "private-room-id",
    "invite-code",
    "function-input",
    "chat-input",
];

#[derive(Default)]
struct FormState {
    inputs: Vec<(String, String)>,
    room_visibility: Option<String>,
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
    socket: Option<WebSocket>,
    socket_handlers: Option<SocketHandlers>,
    connection_epoch: u64,
    auth_epoch: u64,
    auth_pending: bool,
    auth_request: Option<AbortController>,
    event_handlers: Vec<Closure<dyn FnMut(Event)>>,
    reconnect_attempt: u32,
    reconnect_timer: Option<Timeout>,
    clock: Option<Interval>,
    viewport_handler: Option<Closure<dyn FnMut(Event)>>,
    expired_deadline: Option<i64>,
    shot_animation: Option<ShotAnimation>,
    ws_url: String,
}

struct SocketHandlers {
    _onopen: Closure<dyn FnMut(Event)>,
    _onmessage: Closure<dyn FnMut(MessageEvent)>,
    _onclose: Closure<dyn FnMut(CloseEvent)>,
    _onerror: Closure<dyn FnMut(ErrorEvent)>,
}

struct ShotAnimation {
    sequence: u64,
    started_at: f64,
}

type SharedApp = Rc<RefCell<App>>;

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
        socket: None,
        socket_handlers: None,
        connection_epoch: 0,
        auth_epoch: 0,
        auth_pending: false,
        auth_request: None,
        event_handlers: Vec::new(),
        reconnect_attempt: 0,
        reconnect_timer: None,
        clock: None,
        viewport_handler: None,
        expired_deadline: None,
        shot_animation: None,
        ws_url,
    }));

    render(&app)?;
    bind_events(&app)?;
    bind_viewport_events(&app)?;
    let clock_app = Rc::clone(&app);
    app.borrow_mut().clock = Some(Interval::new(1_000, move || update_timer(&clock_app)));
    restore_session(&app);
    Ok(())
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
    {
        let mut app_ref = app.borrow_mut();
        if app_ref.auth_epoch != auth_epoch
            || app_ref.auth_pending
            || app_ref.model.player_id.is_some()
        {
            return;
        }
        reduce(
            &mut app_ref.model,
            Action::Authenticated {
                player_id: account.id.to_string(),
                display_name: account.display_name,
            },
        );
    }
    rerender(app);
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
    let (ws_url, connection_epoch, auth_epoch, previous_socket, previous_handlers) = {
        let mut app = app.borrow_mut();
        if app.model.player_id.is_none() || app.auth_pending {
            return Ok(());
        }
        app.connection_epoch = app.connection_epoch.saturating_add(1);
        app.reconnect_timer = None;
        let previous_socket = app.socket.take();
        let previous_handlers = app.socket_handlers.take();
        reduce(&mut app.model, Action::Connecting);
        (
            app.ws_url.clone(),
            app.connection_epoch,
            app.auth_epoch,
            previous_socket,
            previous_handlers,
        )
    };
    if let Some(socket) = previous_socket {
        dispose_socket(socket, previous_handlers);
    }
    rerender(app);
    let socket = match WebSocket::new(&ws_url) {
        Ok(socket) => socket,
        Err(error) => {
            let mut app_ref = app.borrow_mut();
            let current =
                app_ref.connection_epoch == connection_epoch && app_ref.auth_epoch == auth_epoch;
            if current {
                reduce(&mut app_ref.model, Action::GiveUp);
            }
            drop(app_ref);
            if current {
                rerender(app);
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
            app.reconnect_attempt = 0;
            app.reconnect_timer = None;
            reduce(&mut app.model, Action::Connected);
        }
        send_current(
            &open_app,
            connection_epoch,
            auth_epoch,
            ClientMessage::Hello {
                version: PROTOCOL_VERSION,
            },
        );
        send_current(
            &open_app,
            connection_epoch,
            auth_epoch,
            ClientMessage::ListRooms,
        );
        rerender(&open_app);
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
            Ok(message) => {
                announce_server_message(&message_app, &message);
                let app_ref = message_app.borrow();
                let was_game = app_ref.model.screen == Screen::Game;
                let prior_sequence = app_ref.model.shot_sequence;
                drop(app_ref);
                reduce(
                    &mut message_app.borrow_mut().model,
                    Action::Message(Box::new(message)),
                );
                let app_ref = message_app.borrow();
                let is_game = app_ref.model.screen == Screen::Game;
                let sequence = app_ref.model.shot_sequence;
                drop(app_ref);
                if sequence != prior_sequence {
                    start_shot_animation(&message_app, sequence);
                }
                if was_game && is_game {
                    refresh_game(&message_app);
                } else {
                    rerender(&message_app);
                }
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

fn schedule_reconnect(app: &SharedApp, connection_epoch: u64, auth_epoch: u64) {
    let (attempt, timer_epoch) = {
        let mut app = app.borrow_mut();
        if !connection_is_current(&app, connection_epoch, auth_epoch) {
            return;
        }
        app.socket = None;
        app.socket_handlers = None;
        app.connection_epoch = app.connection_epoch.saturating_add(1);
        let timer_epoch = app.connection_epoch;
        let attempt = app.reconnect_attempt.saturating_add(1);
        app.reconnect_attempt = attempt;
        if attempt > 10 {
            reduce(&mut app.model, Action::GiveUp);
        } else {
            reduce(&mut app.model, Action::Disconnected { attempt });
        }
        (attempt, timer_epoch)
    };
    rerender(app);
    if attempt > 10 {
        return;
    }

    let delay_ms = 500_u32.saturating_mul(2_u32.saturating_pow(attempt.min(5)));
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

fn send(app: &SharedApp, message: ClientMessage) {
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
        return;
    }
    send_current(app, connection_epoch, auth_epoch, message);
}

fn send_current(app: &SharedApp, connection_epoch: u64, auth_epoch: u64, message: ClientMessage) {
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
    }
}

fn login(app: &SharedApp, request: LoginRequest) {
    let Some((auth_epoch, controller)) = begin_authentication(app) else {
        return;
    };
    rerender(app);
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
    rerender(app);
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
    {
        let mut app_ref = app.borrow_mut();
        if app_ref.auth_epoch != auth_epoch || !app_ref.auth_pending {
            return;
        }
        app_ref.auth_pending = false;
        app_ref.auth_request = None;
        reduce(
            &mut app_ref.model,
            Action::Authenticated {
                player_id: account.id.to_string(),
                display_name: account.display_name,
            },
        );
    }
    rerender(app);
    if let Err(error) = connect(app) {
        notice(app, format!("connection failed: {error:?}"));
    }
}

fn session_expired(app: &SharedApp) {
    let (socket, handlers) = {
        let mut app_ref = app.borrow_mut();
        app_ref.auth_epoch = app_ref.auth_epoch.saturating_add(1);
        app_ref.auth_pending = false;
        app_ref.connection_epoch = app_ref.connection_epoch.saturating_add(1);
        app_ref.reconnect_timer = None;
        app_ref.auth_request = None;
        let socket = app_ref.socket.take();
        let handlers = app_ref.socket_handlers.take();
        reduce(&mut app_ref.model, Action::SessionExpired);
        (socket, handlers)
    };
    if let Some(socket) = socket {
        dispose_socket(socket, handlers);
    }
    notice(app, "Session expired; sign in again".into());
}

fn logout(app: &SharedApp) {
    let (auth_epoch, request, socket, handlers) = {
        let mut app_ref = app.borrow_mut();
        app_ref.auth_epoch = app_ref.auth_epoch.saturating_add(1);
        app_ref.auth_pending = false;
        app_ref.connection_epoch = app_ref.connection_epoch.saturating_add(1);
        app_ref.reconnect_timer = None;
        let request = app_ref.auth_request.take();
        let socket = app_ref.socket.take();
        let handlers = app_ref.socket_handlers.take();
        reduce(&mut app_ref.model, Action::LoggedOut);
        (app_ref.auth_epoch, request, socket, handlers)
    };
    if let Some(request) = request {
        request.abort();
    }
    if let Some(socket) = socket {
        dispose_socket(socket, handlers);
    }
    rerender(app);
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
        ServerMessage::Chat { player_id, text } => {
            let name = app
                .borrow()
                .model
                .players
                .iter()
                .find(|player| player.id == player_id.to_string())
                .map(|player| player.name.clone())
                .unwrap_or_else(|| "Player".into());
            Some(format!("{name}: {text}"))
        }
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
        ServerMessage::ShotResolved { shot, .. } => Some(if shot.hits.is_empty() {
            "Shot resolved; no soldiers hit".into()
        } else {
            format!("Shot resolved; {} soldier(s) hit", shot.hits.len())
        }),
        ServerMessage::GameFinished { shot, .. } => Some(match shot.winner_team {
            Some(1) => "Match finished; Team One wins".into(),
            Some(2) => "Match finished; Team Two wins".into(),
            _ => "Match finished; draw".into(),
        }),
        ServerMessage::Error { message, .. } => Some(message.clone()),
        _ => None,
    };
    if let Some(message) = message {
        announce(app, &message);
    }
}

fn notice(app: &SharedApp, message: String) {
    announce(app, &message);
    {
        let mut app = app.borrow_mut();
        app.model.notices.push(message);
        if app.model.notices.len() > 40 {
            app.model.notices.remove(0);
        }
    }
    if app.borrow().model.screen == Screen::Game {
        refresh_game(app);
    } else {
        rerender(app);
    }
}

fn rerender(app: &SharedApp) {
    let form_state = capture_form_state(app);
    if let Err(error) = render(app) {
        log_error(&format!("render failed: {error:?}"));
        return;
    }
    restore_form_state(app, form_state);
    app.borrow_mut().event_handlers.clear();
    if let Err(error) = bind_events(app) {
        log_error(&format!("event binding failed: {error:?}"));
    }
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

        game_element(document, "#turn-timer")?.set_text_content(Some(&timer_text(model)));
        game_element(document, "#game-title")?.set_text_content(Some(&model.room_name));
        game_element(document, ".scoreboard ul")?.set_inner_html(&scoreboard_rows_html(model));
        game_element(document, "#battlefield-summary")?
            .set_text_content(Some(&battlefield_summary(model)));
        game_element(document, ".chat-panel ul")?.set_inner_html(&chat_messages_html(model));
        game_element(document, ".notices")?.set_inner_html(&notice_items_html(model));

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
        game_element(document, ".fire-button span")?.set_text_content(Some(if local_turn {
            "Fire"
        } else {
            "Waiting"
        }));

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
    let room_visibility = app
        .document
        .get_element_by_id("room-visibility")
        .and_then(|element| element.dyn_into::<HtmlSelectElement>().ok())
        .map(|select| select.value());
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
        room_visibility,
        focus,
    }
}

fn focus_selector(element: &web_sys::Element) -> Option<String> {
    let id = element.id();
    if !id.is_empty() {
        return Some(format!("#{id}"));
    }
    if let Some(player_id) = element.get_attribute("data-player-id") {
        return Some(format!(
            "{}[data-player-id=\"{player_id}\"]",
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
    if let Some(value) = state.room_visibility
        && let Some(select) = document
            .get_element_by_id("room-visibility")
            .and_then(|element| element.dyn_into::<HtmlSelectElement>().ok())
    {
        select.set_value(&value);
    }
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
    root.set_inner_html(&format!(
        "<div class=\"app-frame\">{}<main id=\"screen\">{}</main>{}</div>",
        header_html(&app_ref.model),
        screen,
        notices_html(&app_ref.model)
    ));
    drop(app_ref);
    if app.borrow().model.screen == Screen::Game
        && let Err(error) = render_canvas(app)
    {
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
    let (class, label) = match model.connection {
        Connection::Connecting => ("is-waiting", "Connecting".into()),
        Connection::Online => ("is-online", "Online".into()),
        Connection::Reconnecting { attempt } => {
            ("is-waiting", format!("Reconnecting · attempt {attempt}"))
        }
        Connection::Offline => ("is-offline", "Offline".into()),
    };
    let retry = matches!(model.connection, Connection::Offline)
        .then_some("<button id=\"reconnect-now\" class=\"text-button\">Reconnect</button>")
        .unwrap_or("");
    format!(
        "<header class=\"masthead\"><a class=\"wordmark\" href=\"/\" aria-label=\"Graphwar home\"><span>GRAPH</span><strong>WAR</strong></a><div><p class=\"connection {class}\" role=\"status\"><i></i>{}</p>{retry}{account_action}</div></header>",
        escape(&label)
    )
}

fn login_html() -> String {
    "<section class=\"login-shell reveal\" aria-labelledby=\"login-title\"><div class=\"hero-copy\"><p class=\"eyebrow\">Artillery for mathematicians</p><h1 id=\"login-title\">Draw the<br><em>winning line.</em></h1><p>Turn equations into trajectories. Outsmart the other side before the clock runs dry.</p></div><div class=\"auth-stack\"><form id=\"login-form\" class=\"paper-card\"><h2>Return to battle</h2><label for=\"login-email\">Email</label><input id=\"login-email\" type=\"email\" autocomplete=\"email\" maxlength=\"254\" required><label for=\"login-password\">Password</label><input id=\"login-password\" type=\"password\" autocomplete=\"current-password\" minlength=\"12\" required><button class=\"primary\" type=\"submit\">Enter the lobby <span aria-hidden=\"true\">↗</span></button></form><form id=\"register-form\" class=\"paper-card\"><h2>First deployment</h2><label for=\"register-name\">Display name</label><input id=\"register-name\" autocomplete=\"nickname\" minlength=\"2\" maxlength=\"32\" required placeholder=\"e.g. Gauss\"><label for=\"register-email\">Email</label><input id=\"register-email\" type=\"email\" autocomplete=\"email\" maxlength=\"254\" required><label for=\"register-password\">Password</label><input id=\"register-password\" type=\"password\" autocomplete=\"new-password\" minlength=\"12\" required><button class=\"secondary\" type=\"submit\">Create account</button><small>Passwords need at least 12 characters.</small></form></div></section>".into()
}

fn lobby_html(model: &Model) -> String {
    let rooms = if model.rooms.is_empty() {
        "<li class=\"empty\"><strong>No open rooms.</strong><span>Start the first skirmish.</span></li>".into()
    } else {
        model
            .rooms
            .iter()
            .map(|room| format!(
                "<li><div><strong>{}</strong><span>{} / {} players</span></div><button class=\"join-room secondary\" data-room-id=\"{}\">Join <span aria-hidden=\"true\">→</span></button></li>",
                escape(&room.name), room.players, room.capacity, attr(&room.id)
            ))
            .collect::<String>()
    };
    format!(
        "<section class=\"lobby-shell reveal\" aria-labelledby=\"lobby-title\"><div class=\"section-heading\"><div><p class=\"eyebrow\">Welcome, {}</p><h1 id=\"lobby-title\">Open rooms</h1></div><div class=\"lobby-actions\"><form id=\"create-room-form\" class=\"inline-form\"><label class=\"sr-only\" for=\"room-name\">New room name</label><input id=\"room-name\" maxlength=\"32\" required placeholder=\"Room name\"><select id=\"room-visibility\" aria-label=\"Room visibility\"><option value=\"public\">Public</option><option value=\"private\">Private</option></select><button class=\"primary\" type=\"submit\">Create room</button></form><form id=\"invite-room-form\" class=\"inline-form\"><label class=\"sr-only\" for=\"private-room-id\">Private room ID</label><input id=\"private-room-id\" required placeholder=\"Room ID\"><label class=\"sr-only\" for=\"invite-code\">Private invite code</label><input id=\"invite-code\" required placeholder=\"Invite code\"><button class=\"secondary\" type=\"submit\">Join private</button></form></div></div><ul class=\"room-list\">{rooms}</ul></section>",
        escape(&model.player_name)
    )
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
    let players = model
        .players
        .iter()
        .map(|player| {
            let local = model.player_id.as_deref() == Some(player.id.as_str());
            let editable = local || (owner_controls && player.is_bot);
            let controls = editable
                .then(|| {
                    format!(
                        "<div class=\"slot-controls\"><label><span class=\"sr-only\">{} </span>Team <select class=\"player-team\" data-player-id=\"{}\"><option value=\"1\"{}>One</option><option value=\"2\"{}>Two</option></select></label><label><span class=\"sr-only\">{} </span>Soldiers <select class=\"player-soldiers\" data-player-id=\"{}\">{}</select></label>{}</div>",
                        escape(&player.name),
                        attr(&player.id),
                        (player.team == 1).then_some(" selected").unwrap_or(""),
                        (player.team == 2).then_some(" selected").unwrap_or(""),
                        escape(&player.name),
                        attr(&player.id),
                        (1..=4)
                            .map(|count| format!(
                                "<option value=\"{count}\"{}>{count}</option>",
                                (player.soldiers == count).then_some(" selected").unwrap_or("")
                            ))
                            .collect::<String>(),
                        player
                            .is_bot
                            .then(|| format!(
                                "<button class=\"remove-bot text-button\" data-player-id=\"{}\" aria-label=\"Remove {}\">Remove</button>",
                                attr(&player.id),
                                attr(&player.name)
                            ))
                            .unwrap_or_default()
                    )
                })
                .unwrap_or_default();
            format!(
                "<li><span class=\"team team-{}\" aria-hidden=\"true\"></span><strong>{}</strong><span class=\"team-name\">{}</span>{}<span class=\"ready-state\">{}</span>{}</li>",
                player.team,
                escape(&player.name),
                team_name(player.team),
                if player.owner { "<small>Owner</small>" } else if player.is_bot { "<small>Computer</small>" } else { "" },
                if player.ready { "Ready" } else { "Plotting" },
                controls
            )
        })
        .collect::<String>();
    format!(
        "<section class=\"room-shell reveal\" aria-labelledby=\"room-title\"><div class=\"section-heading\"><div><p class=\"eyebrow\">Staging area</p><h1 id=\"room-title\">{}</h1></div><button id=\"leave-room\" class=\"text-button\">Leave room</button></div><div class=\"room-grid\"><section class=\"paper-card roster\" aria-labelledby=\"players-title\"><h2 id=\"players-title\">Players <span>{}</span></h2><ul>{players}</ul></section><aside class=\"briefing\"><p>Configure your slot, then ready up. The owner starts after everyone commits.</p><fieldset class=\"mode-picker\"{}><legend>Rule set</legend><label><input type=\"radio\" name=\"game-mode\" value=\"function\"{}> Function</label><label><input type=\"radio\" name=\"game-mode\" value=\"first_order\"{}> First-order</label><label><input type=\"radio\" name=\"game-mode\" value=\"second_order\"{}> Second-order</label></fieldset><button id=\"ready-button\" class=\"primary wide\">{ready_label}</button><button id=\"add-bot\" class=\"text-button wide\"{}>Add computer</button><button id=\"start-game\" class=\"secondary wide\"{}>Start match</button></aside></div>{}</section>",
        escape(&model.room_name),
        model.players.len(),
        if owner_controls { "" } else { " disabled" },
        mode_checked(GameMode::Function),
        mode_checked(GameMode::FirstOrder),
        mode_checked(GameMode::SecondOrder),
        if owner_controls { "" } else { " disabled" },
        start_disabled,
        chat_html(model)
    )
}

fn game_html(model: &Model) -> String {
    let local_turn = local_turn(model);
    let disabled = (!local_turn).then_some(" disabled").unwrap_or("");
    let second_order = model.game_mode == Some(GameMode::SecondOrder);
    let angle_hidden = (!second_order).then_some(" hidden").unwrap_or("");
    let angle_disabled = (!second_order).then_some(" disabled").unwrap_or(disabled);
    format!(
        "<section class=\"game-shell reveal\" aria-labelledby=\"game-title\"><div class=\"game-heading\"><div><p class=\"eyebrow\">Live match · <span id=\"turn-timer\" role=\"timer\">{}</span></p><h1 id=\"game-title\">{}</h1></div><button id=\"leave-room\" class=\"text-button\">Retreat</button></div><div class=\"battlefield\"><canvas id=\"game-canvas\" width=\"770\" height=\"450\" aria-label=\"Graphwar battlefield\" aria-describedby=\"battlefield-summary\"></canvas><div class=\"preview-key\"><i></i> Provisional</div><div class=\"axis-label x-label\">x</div><div class=\"axis-label y-label\">y</div></div><p id=\"battlefield-summary\" class=\"sr-only\">{}</p>{}<form id=\"fire-form\" class=\"fire-console\"><div class=\"equation-field\"><label for=\"function-input\">Function</label><div><span aria-hidden=\"true\">y =</span><input id=\"function-input\" spellcheck=\"false\" autocomplete=\"off\" maxlength=\"256\" required value=\"{}\" aria-describedby=\"function-hint function-error\"{disabled}></div><small id=\"function-hint\">Use x, sin, cos, tan, sqrt and standard operators.</small><p id=\"function-error\" class=\"function-error\" aria-live=\"polite\"></p></div><div class=\"angle-field\"{angle_hidden}><div class=\"angle-label\"><label for=\"angle-input\">Launch angle</label><output id=\"angle-output\" for=\"angle-input\">{:.1}°</output></div><input id=\"angle-input\" type=\"range\" min=\"-90\" max=\"90\" value=\"{:.1}\" step=\"0.1\" aria-describedby=\"angle-hint angle-output\"{angle_disabled}><small id=\"angle-hint\">Focus the slider, then use Arrow Up/Down.</small></div><button class=\"fire-button\" type=\"submit\"{disabled}><span>{}</span><small>Enter ↵</small></button></form>{}</section>",
        timer_text(model),
        escape(&model.room_name),
        escape(&battlefield_summary(model)),
        scoreboard_html(model),
        escape(if model.draft_function.is_empty() {
            "sin(x)"
        } else {
            &model.draft_function
        }),
        model.aim_angle_deg,
        model.aim_angle_deg,
        if local_turn { "Fire" } else { "Waiting" },
        chat_html(model)
    )
}

fn scoreboard_html(model: &Model) -> String {
    format!(
        "<section class=\"scoreboard paper-card\" aria-label=\"Match scoreboard\"><h2>Field report</h2><ul>{}</ul></section>",
        scoreboard_rows_html(model)
    )
}

fn scoreboard_rows_html(model: &Model) -> String {
    model
        .players
        .iter()
        .map(|player| {
            let alive = model
                .soldiers
                .iter()
                .filter(|soldier| soldier.player_id == player.id && soldier.alive)
                .count();
            let turn = (model.turn_player_id.as_deref() == Some(player.id.as_str()))
                .then_some("<span class=\"turn-mark\">Turn</span>")
                .unwrap_or("");
            format!(
                "<li><span class=\"team team-{}\" aria-hidden=\"true\"></span><strong>{}</strong><span class=\"team-name\">{}</span><span>{alive} / {}</span>{turn}</li>",
                player.team,
                escape(&player.name),
                team_name(player.team),
                player.soldiers
            )
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

fn timer_text(model: &Model) -> String {
    let Some(deadline) = model.turn_deadline_at else {
        return match model.room_phase {
            Some(graphwar_protocol::Phase::Resolving) => "Resolving shot".into(),
            Some(graphwar_protocol::Phase::Finished) => "Match finished".into(),
            _ => "Waiting".into(),
        };
    };
    let remaining = deadline.saturating_sub(unix_time());
    format!("{}:{:02}", remaining / 60, remaining % 60)
}

fn update_timer(app: &SharedApp) {
    let should_rerender = {
        let mut app_ref = app.borrow_mut();
        let deadline = app_ref.model.turn_deadline_at;
        let expired = app_ref.model.screen == Screen::Game
            && app_ref.model.room_phase == Some(graphwar_protocol::Phase::Planning)
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
    let app_ref = app.borrow();
    if app_ref.model.screen != Screen::Game {
        return;
    }
    if let Some(timer) = app_ref.document.get_element_by_id("turn-timer") {
        timer.set_text_content(Some(&timer_text(&app_ref.model)));
    }
}

fn bind_viewport_events(app: &SharedApp) -> Result<(), JsValue> {
    let window = app.borrow().window.clone();
    let redraw_app = Rc::clone(app);
    let handler = Closure::<dyn FnMut(Event)>::new(move |_| {
        if redraw_app.borrow().model.screen == Screen::Game
            && let Err(error) = render_canvas(&redraw_app)
        {
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
    let hits = (!model.shot_hits.is_empty())
        .then(|| format!(" {} soldier(s) hit.", model.shot_hits.len()))
        .unwrap_or_default();
    let explosion = model
        .shot_explosion
        .as_ref()
        .map_or_else(String::new, |explosion| {
            format!(" Explosion at ({:.0}, {:.0}).", explosion.x, explosion.y)
        });
    format!(
        "Battlefield: Team One has {team_one} living soldier(s); Team Two has {team_two}. {active}{hits}{explosion}"
    )
}

fn chat_html(model: &Model) -> String {
    format!(
        "<section class=\"paper-card chat-panel\" aria-labelledby=\"chat-title\"><h2 id=\"chat-title\">Room chat</h2><ul>{}</ul><form id=\"chat-form\" class=\"inline-form\"><label class=\"sr-only\" for=\"chat-input\">Message</label><input id=\"chat-input\" maxlength=\"500\" autocomplete=\"off\" required placeholder=\"Message the room\"><button class=\"secondary\" type=\"submit\">Send</button></form></section>",
        chat_messages_html(model)
    )
}

fn chat_messages_html(model: &Model) -> String {
    model
        .chat
        .iter()
        .rev()
        .take(40)
        .rev()
        .map(|message| {
            let name = model
                .players
                .iter()
                .find(|player| player.id == message.player_id)
                .map(|player| player.name.as_str())
                .unwrap_or("Player");
            format!(
                "<li><strong>{}</strong><span>{}</span></li>",
                escape(name),
                escape(&message.text)
            )
        })
        .collect()
}

fn notices_html(model: &Model) -> String {
    format!("<ul class=\"notices\">{}</ul>", notice_items_html(model))
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
                send(&app, ClientMessage::CreateRoom { name, visibility });
            }
        });
    }
    if let Some(form) = document.get_element_by_id("invite-room-form") {
        let app = Rc::clone(app);
        bind_submit(&app.clone(), form.unchecked_into(), move |form| {
            let Some(room_id) =
                input_value(&form, "private-room-id").and_then(|id| Uuid::parse_str(&id).ok())
            else {
                notice(&app, "Invalid private room ID".into());
                return;
            };
            let Some(invite) = input_value(&form, "invite-code") else {
                return;
            };
            send(
                &app,
                ClientMessage::JoinRoom {
                    room_id,
                    invite: Some(invite),
                },
            );
        });
    }
    let room_buttons = document.query_selector_all(".join-room")?;
    for index in 0..room_buttons.length() {
        let Some(element) = room_buttons.item(index) else {
            continue;
        };
        let element = element.unchecked_into::<web_sys::Element>();
        let room_id = element.get_attribute("data-room-id").unwrap_or_default();
        let app = Rc::clone(app);
        bind_click(&app.clone(), &element, move || {
            match Uuid::parse_str(&room_id) {
                Ok(room_id) => send(
                    &app,
                    ClientMessage::JoinRoom {
                        room_id,
                        invite: None,
                    },
                ),
                Err(_) => log_error("invalid room ID"),
            }
        });
    }
    if let Some(button) = document.get_element_by_id("leave-room") {
        let app = Rc::clone(app);
        bind_click(&app.clone(), &button, move || {
            send(&app, ClientMessage::LeaveRoom);
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
            send(&app, ClientMessage::AddBot { level: 4 })
        });
    }
    if let Some(button) = document.get_element_by_id("start-game") {
        let app = Rc::clone(app);
        bind_click(&app.clone(), &button, move || {
            send(&app, ClientMessage::StartGame)
        });
    }
    let bot_buttons = document.query_selector_all(".remove-bot")?;
    for index in 0..bot_buttons.length() {
        let Some(element) = bot_buttons.item(index) else {
            continue;
        };
        let element = element.unchecked_into::<web_sys::Element>();
        let player_id = element.get_attribute("data-player-id").unwrap_or_default();
        let app = Rc::clone(app);
        bind_click(&app.clone(), &element, move || {
            if let Ok(player_id) = Uuid::parse_str(&player_id) {
                send(&app, ClientMessage::RemoveBot { player_id });
            }
        });
    }
    for selector in [".player-team", ".player-soldiers"] {
        let inputs = document.query_selector_all(selector)?;
        for index in 0..inputs.length() {
            let Some(element) = inputs.item(index) else {
                continue;
            };
            let element = element.unchecked_into::<HtmlSelectElement>();
            let player_id = element.get_attribute("data-player-id").unwrap_or_default();
            let app = Rc::clone(app);
            bind_select_change(&app.clone(), &element, move |input| {
                let Ok(player_id) = Uuid::parse_str(&player_id) else {
                    return;
                };
                let value = input.value().parse::<u8>().unwrap_or_default();
                if selector == ".player-team" {
                    send(
                        &app,
                        ClientMessage::SetTeam {
                            player_id,
                            team: value,
                        },
                    );
                } else {
                    send(
                        &app,
                        ClientMessage::SetSoldiers {
                            player_id,
                            soldiers: value,
                        },
                    );
                }
            })?;
        }
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
        input.add_event_listener_with_callback("input", closure.as_ref().unchecked_ref())?;
        app.borrow_mut().event_handlers.push(closure);
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
        input.add_event_listener_with_callback("input", closure.as_ref().unchecked_ref())?;
        app.borrow_mut().event_handlers.push(closure);
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
    function.set_custom_validity(error.unwrap_or(""));
    if let Some(message) = document.get_element_by_id("function-error") {
        message.set_text_content(error);
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

fn retain_event_handler(app: &SharedApp, closure: Closure<dyn FnMut(Event)>) {
    app.borrow_mut().event_handlers.push(closure);
}

fn bind_submit(
    app: &SharedApp,
    form: HtmlFormElement,
    mut handler: impl FnMut(HtmlFormElement) + 'static,
) {
    let bound_form = form.clone();
    let closure = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        event.prevent_default();
        handler(bound_form.clone());
    });
    let _ = form.add_event_listener_with_callback("submit", closure.as_ref().unchecked_ref());
    retain_event_handler(app, closure);
}

fn bind_click(app: &SharedApp, element: &web_sys::Element, mut handler: impl FnMut() + 'static) {
    let closure = Closure::<dyn FnMut(Event)>::new(move |_| handler());
    let _ = element.add_event_listener_with_callback("click", closure.as_ref().unchecked_ref());
    retain_event_handler(app, closure);
}

fn bind_change(
    app: &SharedApp,
    input: &HtmlInputElement,
    mut handler: impl FnMut(HtmlInputElement) + 'static,
) -> Result<(), JsValue> {
    let bound_input = input.clone();
    let closure = Closure::<dyn FnMut(Event)>::new(move |_| handler(bound_input.clone()));
    input.add_event_listener_with_callback("change", closure.as_ref().unchecked_ref())?;
    retain_event_handler(app, closure);
    Ok(())
}

fn bind_select_change(
    app: &SharedApp,
    input: &HtmlSelectElement,
    mut handler: impl FnMut(HtmlSelectElement) + 'static,
) -> Result<(), JsValue> {
    let bound_input = input.clone();
    let closure = Closure::<dyn FnMut(Event)>::new(move |_| handler(bound_input.clone()));
    input.add_event_listener_with_callback("change", closure.as_ref().unchecked_ref())?;
    retain_event_handler(app, closure);
    Ok(())
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
    context.set_fill_style_str("#f3eddc");
    context.fill_rect(0.0, 0.0, LOGICAL_WIDTH, LOGICAL_HEIGHT);
    draw_grid(&context);
    draw_terrain(&context, &app.model);
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
    )?;
    draw_path(&context, &app.model.preview_path, true)?;
    for soldier in &app.model.soldiers {
        draw_soldier(
            &context,
            soldier.x,
            soldier.y,
            soldier.team,
            soldier.alive,
            soldier.active,
        );
    }
    if authoritative_len == app.model.authoritative_path.len() {
        draw_shot_effects(&context, &app.model);
    }
    Ok(())
}

fn draw_shot_effects(context: &CanvasRenderingContext2d, model: &Model) {
    context.save();
    context.set_stroke_style_str("#ff5b3d");
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

fn draw_grid(context: &CanvasRenderingContext2d) {
    context.set_stroke_style_str("rgba(28, 31, 27, .10)");
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
    context.set_stroke_style_str("#1c1f1b");
    context.set_line_width(1.5);
    context.begin_path();
    context.move_to(0.0, 225.0);
    context.line_to(LOGICAL_WIDTH, 225.0);
    context.move_to(385.0, 0.0);
    context.line_to(385.0, LOGICAL_HEIGHT);
    context.stroke();
}

fn draw_terrain(context: &CanvasRenderingContext2d, model: &Model) {
    context.set_fill_style_str("#244a3b");
    context.set_stroke_style_str("#1c1f1b");
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
        context.set_fill_style_str("#f3eddc");
        context.fill();
        context.save();
        context.clip();
        draw_grid(context);
        context.restore();
    }
}

fn draw_path(
    context: &CanvasRenderingContext2d,
    path: &[(f64, f64)],
    provisional: bool,
) -> Result<(), JsValue> {
    let Some((start, rest)) = path.split_first() else {
        return Ok(());
    };
    context.begin_path();
    context.move_to(start.0, start.1);
    for point in rest {
        context.line_to(point.0, point.1);
    }
    context.set_stroke_style_str(if provisional { "#6d7168" } else { "#ff5b3d" });
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
) {
    let color = if alive {
        if team % 2 == 0 { "#ff5b3d" } else { "#e4b83b" }
    } else {
        "#6d7368"
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
    context.set_stroke_style_str("#1c1f1b");
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
