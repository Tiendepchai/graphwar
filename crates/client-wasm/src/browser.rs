use std::{cell::RefCell, rc::Rc};

use gloo_timers::callback::Timeout;
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use web_sys::{
    CanvasRenderingContext2d, CloseEvent, Document, ErrorEvent, Event, HtmlCanvasElement,
    HtmlFormElement, HtmlInputElement, MessageEvent, WebSocket, Window,
};

use crate::{
    geometry::{LOGICAL_HEIGHT, LOGICAL_WIDTH, Viewport},
    state::{Action, ClientMessage, Connection, Model, Screen, ServerMessage, reduce},
};

struct App {
    window: Window,
    document: Document,
    model: Model,
    player_name: String,
    socket: Option<WebSocket>,
    reconnect_attempt: u32,
    reconnect_timer: Option<Timeout>,
    ws_url: String,
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
        player_name: String::new(),
        socket: None,
        reconnect_attempt: 0,
        reconnect_timer: None,
        ws_url,
    }));

    render(&app)?;
    bind_events(&app)?;
    connect(&app)?;
    Ok(())
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

fn connect(app: &SharedApp) -> Result<(), JsValue> {
    reduce(&mut app.borrow_mut().model, Action::Connecting);
    rerender(app);
    let socket = WebSocket::new(&app.borrow().ws_url)?;

    let open_app = Rc::clone(app);
    let onopen = Closure::<dyn FnMut(Event)>::new(move |_| {
        let mut app = open_app.borrow_mut();
        app.reconnect_attempt = 0;
        reduce(&mut app.model, Action::Connected);
        let name = app.player_name.trim().to_owned();
        drop(app);
        if !name.is_empty() {
            send(&open_app, ClientMessage::Login { name });
        }
        rerender(&open_app);
    });
    socket.set_onopen(Some(onopen.as_ref().unchecked_ref()));
    onopen.forget();

    let message_app = Rc::clone(app);
    let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        let Some(text) = event.data().as_string() else {
            return;
        };
        match serde_json::from_str::<ServerMessage>(&text) {
            Ok(message) => {
                reduce(
                    &mut message_app.borrow_mut().model,
                    Action::Message(message),
                );
                rerender(&message_app);
            }
            Err(error) => log_error(&format!("protocol error: {error}")),
        }
    });
    socket.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
    onmessage.forget();

    let close_app = Rc::clone(app);
    let onclose = Closure::<dyn FnMut(CloseEvent)>::new(move |_| {
        schedule_reconnect(&close_app);
    });
    socket.set_onclose(Some(onclose.as_ref().unchecked_ref()));
    onclose.forget();

    let onerror = Closure::<dyn FnMut(ErrorEvent)>::new(move |event: ErrorEvent| {
        log_error(&format!("WebSocket error: {}", event.message()));
    });
    socket.set_onerror(Some(onerror.as_ref().unchecked_ref()));
    onerror.forget();

    app.borrow_mut().socket = Some(socket);
    Ok(())
}

fn schedule_reconnect(app: &SharedApp) {
    let attempt = app.borrow().reconnect_attempt.saturating_add(1).min(10);
    {
        let mut app = app.borrow_mut();
        app.socket = None;
        app.reconnect_attempt = attempt;
        reduce(&mut app.model, Action::Disconnected { attempt });
    }
    rerender(app);

    let delay_ms = 500_u32.saturating_mul(2_u32.saturating_pow(attempt.min(5)));
    let reconnect_app = Rc::clone(app);
    let timer = Timeout::new(delay_ms, move || {
        if let Err(error) = connect(&reconnect_app) {
            log_error(&format!("reconnect failed: {error:?}"));
            schedule_reconnect(&reconnect_app);
        }
    });
    app.borrow_mut().reconnect_timer = Some(timer);
}

fn send(app: &SharedApp, message: ClientMessage) {
    let result = serde_json::to_string(&message)
        .map_err(|error| JsValue::from_str(&error.to_string()))
        .and_then(|json| {
            app.borrow()
                .socket
                .as_ref()
                .ok_or_else(|| JsValue::from_str("connection unavailable"))?
                .send_with_str(&json)
        });
    if let Err(error) = result {
        log_error(&format!("send failed: {error:?}"));
    }
}

fn rerender(app: &SharedApp) {
    if let Err(error) = render(app).and_then(|()| bind_events(app)) {
        log_error(&format!("render failed: {error:?}"));
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
    if app.borrow().model.screen == Screen::Game {
        render_canvas(app)?;
    }
    Ok(())
}

fn header_html(model: &Model) -> String {
    let (class, label) = match model.connection {
        Connection::Connecting => ("is-waiting", "Connecting".into()),
        Connection::Online => ("is-online", "Online".into()),
        Connection::Reconnecting { attempt } => {
            ("is-waiting", format!("Reconnecting · attempt {attempt}"))
        }
        Connection::Offline => ("is-offline", "Offline".into()),
    };
    format!(
        "<header class=\"masthead\"><a class=\"wordmark\" href=\"/\" aria-label=\"Graphwar home\"><span>GRAPH</span><strong>WAR</strong></a><p class=\"connection {class}\" role=\"status\"><i></i>{}</p></header>",
        escape(&label)
    )
}

fn login_html() -> String {
    "<section class=\"login-shell reveal\" aria-labelledby=\"login-title\"><div class=\"hero-copy\"><p class=\"eyebrow\">Artillery for mathematicians</p><h1 id=\"login-title\">Draw the<br><em>winning line.</em></h1><p>Turn equations into trajectories. Outsmart the other side before the clock runs dry.</p></div><form id=\"login-form\" class=\"paper-card\"><label for=\"player-name\">Call sign</label><input id=\"player-name\" name=\"name\" autocomplete=\"nickname\" minlength=\"2\" maxlength=\"24\" required placeholder=\"e.g. Gauss\"><button class=\"primary\" type=\"submit\">Enter the lobby <span aria-hidden=\"true\">↗</span></button><small>No download. Keyboard friendly. Mildly competitive.</small></form></section>".into()
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
        "<section class=\"lobby-shell reveal\" aria-labelledby=\"lobby-title\"><div class=\"section-heading\"><div><p class=\"eyebrow\">Welcome, {}</p><h1 id=\"lobby-title\">Open rooms</h1></div><form id=\"create-room-form\" class=\"inline-form\"><label class=\"sr-only\" for=\"room-name\">New room name</label><input id=\"room-name\" maxlength=\"32\" required placeholder=\"Room name\"><button class=\"primary\" type=\"submit\">Create room</button></form></div><ul class=\"room-list\">{rooms}</ul></section>",
        escape(&model.player_name)
    )
}

fn room_html(model: &Model) -> String {
    let players = model
        .players
        .iter()
        .map(|player| format!(
            "<li><span class=\"team team-{}\" aria-hidden=\"true\"></span><strong>{}</strong><span class=\"ready-state\">{}</span></li>",
            player.team,
            escape(&player.name),
            if player.ready { "Ready" } else { "Plotting" }
        ))
        .collect::<String>();
    format!(
        "<section class=\"room-shell reveal\" aria-labelledby=\"room-title\"><div class=\"section-heading\"><div><p class=\"eyebrow\">Staging area</p><h1 id=\"room-title\">{}</h1></div><button id=\"leave-room\" class=\"text-button\">Leave room</button></div><div class=\"room-grid\"><section class=\"paper-card roster\" aria-labelledby=\"players-title\"><h2 id=\"players-title\">Players <span>{}</span></h2><ul>{players}</ul></section><aside class=\"briefing\"><p>Choose your side. Ready up. The match begins when everyone commits.</p><button id=\"ready-button\" class=\"primary wide\">I’m ready</button></aside></div></section>",
        escape(&model.room_name),
        model.players.len()
    )
}

fn game_html(model: &Model) -> String {
    format!(
        "<section class=\"game-shell reveal\" aria-labelledby=\"game-title\"><div class=\"game-heading\"><div><p class=\"eyebrow\">Live match</p><h1 id=\"game-title\">{}</h1></div><button id=\"leave-room\" class=\"text-button\">Retreat</button></div><div class=\"battlefield\"><canvas id=\"game-canvas\" width=\"770\" height=\"450\" aria-label=\"Graphwar battlefield, 770 by 450 logical units\"></canvas><div class=\"axis-label x-label\">x</div><div class=\"axis-label y-label\">y</div></div><form id=\"fire-form\" class=\"fire-console\"><div class=\"equation-field\"><label for=\"function-input\">Function</label><div><span aria-hidden=\"true\">y =</span><input id=\"function-input\" spellcheck=\"false\" autocomplete=\"off\" maxlength=\"160\" required value=\"sin(x)\" aria-describedby=\"function-hint\"></div><small id=\"function-hint\">Use x, sin, cos, tan, sqrt and standard operators.</small></div><div class=\"angle-field\"><label for=\"angle-input\">Angle <output id=\"angle-output\">45°</output></label><input id=\"angle-input\" type=\"range\" min=\"-90\" max=\"90\" value=\"45\" step=\"1\"></div><button class=\"fire-button\" type=\"submit\"><span>Fire</span><small>Enter ↵</small></button></form></section>",
        escape(&model.room_name)
    )
}

fn notices_html(model: &Model) -> String {
    let notices = model
        .notices
        .iter()
        .rev()
        .take(3)
        .map(|notice| format!("<li>{}</li>", escape(notice)))
        .collect::<String>();
    format!("<ul class=\"notices\" aria-live=\"polite\">{notices}</ul>")
}

fn bind_events(app: &SharedApp) -> Result<(), JsValue> {
    let document = app.borrow().document.clone();
    if let Some(form) = document.get_element_by_id("login-form") {
        let app = Rc::clone(app);
        bind_submit(form.unchecked_into(), move |form| {
            if let Some(name) = input_value(&form, "player-name") {
                app.borrow_mut().player_name = name.clone();
                send(&app, ClientMessage::Login { name });
            }
        });
    }
    if let Some(form) = document.get_element_by_id("create-room-form") {
        let app = Rc::clone(app);
        bind_submit(form.unchecked_into(), move |form| {
            if let Some(name) = input_value(&form, "room-name") {
                send(&app, ClientMessage::CreateRoom { name });
            }
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
        bind_click(&element, move || {
            send(
                &app,
                ClientMessage::JoinRoom {
                    room_id: room_id.clone(),
                },
            );
        });
    }
    if let Some(button) = document.get_element_by_id("leave-room") {
        let app = Rc::clone(app);
        bind_click(&button, move || {
            send(&app, ClientMessage::LeaveRoom);
            reduce(&mut app.borrow_mut().model, Action::LeftRoom);
            rerender(&app);
        });
    }
    if let Some(button) = document.get_element_by_id("ready-button") {
        let app = Rc::clone(app);
        bind_click(&button, move || {
            send(&app, ClientMessage::SetReady { ready: true })
        });
    }
    if let Some(input) = document.get_element_by_id("angle-input") {
        let document = document.clone();
        let input = input.unchecked_into::<HtmlInputElement>();
        let listener_input = input.clone();
        let closure = Closure::<dyn FnMut(Event)>::new(move |_| {
            if let Some(output) = document.get_element_by_id("angle-output") {
                output.set_text_content(Some(&format!("{}°", listener_input.value())));
            }
        });
        input.add_event_listener_with_callback("input", closure.as_ref().unchecked_ref())?;
        closure.forget();
    }
    if let Some(form) = document.get_element_by_id("fire-form") {
        let app = Rc::clone(app);
        bind_submit(form.unchecked_into(), move |form| {
            let Some(function) = input_value(&form, "function-input") else {
                return;
            };
            let angle_deg = input_value(&form, "angle-input")
                .and_then(|value| value.parse::<f64>().ok())
                .unwrap_or_default()
                .clamp(-90.0, 90.0);
            send(
                &app,
                ClientMessage::Fire {
                    function,
                    angle_deg,
                },
            );
        });
    }
    Ok(())
}

fn bind_submit(form: HtmlFormElement, mut handler: impl FnMut(HtmlFormElement) + 'static) {
    let bound_form = form.clone();
    let closure = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        event.prevent_default();
        handler(bound_form.clone());
    });
    let _ = form.add_event_listener_with_callback("submit", closure.as_ref().unchecked_ref());
    closure.forget();
}

fn bind_click(element: &web_sys::Element, mut handler: impl FnMut() + 'static) {
    let closure = Closure::<dyn FnMut(Event)>::new(move |_| handler());
    let _ = element.add_event_listener_with_callback("click", closure.as_ref().unchecked_ref());
    closure.forget();
}

fn input_value(form: &HtmlFormElement, id: &str) -> Option<String> {
    let input = form.query_selector(&format!("#{id}")).ok()??;
    let value = input.dyn_into::<HtmlInputElement>().ok()?.value();
    let value = value.trim();
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
    draw_terrain(&context);
    for soldier in &app.model.soldiers {
        draw_soldier(
            &context,
            soldier.x,
            soldier.y,
            soldier.team,
            soldier.health,
            soldier.active,
        );
    }
    Ok(())
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

fn draw_terrain(context: &CanvasRenderingContext2d) {
    context.begin_path();
    context.move_to(0.0, 345.0);
    context.bezier_curve_to(130.0, 295.0, 210.0, 385.0, 330.0, 335.0);
    context.bezier_curve_to(450.0, 285.0, 555.0, 375.0, 770.0, 315.0);
    context.line_to(770.0, 450.0);
    context.line_to(0.0, 450.0);
    context.close_path();
    context.set_fill_style_str("#244a3b");
    context.fill();
    context.set_stroke_style_str("#1c1f1b");
    context.set_line_width(3.0);
    context.stroke();
}

fn draw_soldier(
    context: &CanvasRenderingContext2d,
    x: f64,
    y: f64,
    team: u8,
    health: u16,
    active: bool,
) {
    let color = if team % 2 == 0 { "#ff5b3d" } else { "#e4b83b" };
    context.begin_path();
    let _ = context.arc(
        x,
        y - 8.0,
        if active { 7.0 } else { 5.0 },
        0.0,
        std::f64::consts::TAU,
    );
    context.set_fill_style_str(color);
    context.fill();
    context.set_stroke_style_str("#1c1f1b");
    context.set_line_width(if active { 2.5 } else { 1.5 });
    context.stroke();
    context.set_fill_style_str("#1c1f1b");
    context.fill_rect(
        x - 11.0,
        y - 22.0,
        22.0 * f64::from(health.min(100)) / 100.0,
        2.0,
    );
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
