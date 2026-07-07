//! Web (wasm32-unknown-emscripten) client.
//!
//! raylib's web target is emscripten, but spacetimedb-sdk's browser support
//! needs wasm-bindgen, which only targets wasm32-unknown-unknown — the two
//! can't live in one binary. So instead of a live WebSocket subscription,
//! this binary polls SpacetimeDB's plain HTTP API (POST /call, POST /sql)
//! via a synchronous XMLHttpRequest injected through
//! emscripten_run_script_string. See README.web.md.

use raylib::prelude::*;
use serde::Deserialize;
use std::ffi::{c_void, CStr, CString};
use std::os::raw::c_char;

/// SpacetimeDB host. Point this at your deployed instance before shipping
/// the web build — "localhost" only works when the browser and the
/// SpacetimeDB instance are on the same machine.
const HOST: &str = "http://localhost:3000";
const DB_NAME: &str = "hexmerge";
const HEX_RADIUS: f32 = 28.0;
/// Minimum time between set_pos calls (also our presence heartbeat).
const SEND_INTERVAL: f64 = 0.15;
/// Minimum time between full user-list polls.
const POLL_INTERVAL: f64 = 0.3;
/// Mirrors server/src/lib.rs's PRESENCE_TIMEOUT.
const PRESENCE_TIMEOUT_SECS: i64 = 3;

unsafe extern "C" {
    fn emscripten_run_script_string(script: *const c_char) -> *const c_char;
    fn emscripten_set_main_loop_arg(
        func: extern "C" fn(*mut c_void),
        arg: *mut c_void,
        fps: i32,
        simulate_infinite_loop: bool,
    );
    fn emscripten_get_now() -> f64;
}

#[derive(Deserialize, Default)]
struct HttpResponse {
    #[serde(default)]
    token: String,
    #[serde(default)]
    identity: String,
    #[serde(default)]
    body: String,
}

#[derive(Clone)]
struct UserRow {
    identity_hex: String,
    x: f32,
    y: f32,
    last_seen_micros: i64,
}

struct State {
    rl: RaylibHandle,
    thread: RaylibThread,
    token: Option<String>,
    my_identity: Option<String>,
    users: Vec<UserRow>,
    last_send: f64,
    last_poll: f64,
}

fn run_js(js: &str) -> String {
    let Ok(cjs) = CString::new(js) else {
        return String::new();
    };
    unsafe {
        let ptr = emscripten_run_script_string(cjs.as_ptr());
        if ptr.is_null() {
            return String::new();
        }
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}

/// Synchronous HTTP POST via a browser XMLHttpRequest, run to completion
/// before returning. Simple and reliable for a jam PoC, at the cost of
/// blocking the render loop for the duration of the round trip.
fn http_post(path: &str, body: &str, content_type: &str, token: &Option<String>) -> HttpResponse {
    let url = format!("{HOST}/v1/database/{DB_NAME}{path}");
    let js_url = serde_json::to_string(&url).unwrap_or_default();
    let js_body = serde_json::to_string(body).unwrap_or_default();
    let js_ct = serde_json::to_string(content_type).unwrap_or_default();
    let js_token = match token {
        Some(t) => serde_json::to_string(t).unwrap_or_else(|_| "null".to_string()),
        None => "null".to_string(),
    };
    let script = format!(
        r#"(function() {{
            try {{
                var xhr = new XMLHttpRequest();
                xhr.open('POST', {js_url}, false);
                xhr.setRequestHeader('Content-Type', {js_ct});
                var tok = {js_token};
                if (tok) xhr.setRequestHeader('Authorization', 'Bearer ' + tok);
                xhr.send({js_body});
                return JSON.stringify({{
                    token: xhr.getResponseHeader('spacetime-identity-token') || '',
                    identity: xhr.getResponseHeader('spacetime-identity') || '',
                    body: xhr.responseText
                }});
            }} catch (e) {{
                return JSON.stringify({{token: '', identity: '', body: ''}});
            }}
        }})()"#
    );
    let raw = run_js(&script);
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Parses the SATS-JSON response of `SELECT * FROM user`. Row shape is a
/// positional array matching our schema: [identity, name, online, x, y, last_seen].
/// Identity and Timestamp are single-element arrays (SATS newtype wrappers).
fn parse_users(sql_body: &str) -> Vec<UserRow> {
    let mut out = Vec::new();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(sql_body) else {
        return out;
    };
    let Some(rows) = v
        .get(0)
        .and_then(|stmt| stmt.get("rows"))
        .and_then(|r| r.as_array())
    else {
        return out;
    };
    for row in rows {
        let Some(arr) = row.as_array() else { continue };
        let identity_hex = arr
            .get(0)
            .and_then(|v| v.as_array())
            .and_then(|a| a.get(0))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let x = arr.get(3).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let y = arr.get(4).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let last_seen_micros = arr
            .get(5)
            .and_then(|v| v.as_array())
            .and_then(|a| a.get(0))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if identity_hex.is_empty() {
            continue;
        }
        out.push(UserRow {
            identity_hex,
            x,
            y,
            last_seen_micros,
        });
    }
    out
}

fn now_secs() -> f64 {
    unsafe { emscripten_get_now() / 1000.0 }
}

fn now_micros_since_unix_epoch() -> i64 {
    // The XHR round trip gives us no server clock directly, so fall back to
    // the browser's wall clock — good enough for a few-seconds presence window.
    run_js("Date.now() * 1000")
        .parse::<i64>()
        .unwrap_or(0)
}

fn is_present(last_seen_micros: i64, now_micros: i64) -> bool {
    now_micros.saturating_sub(last_seen_micros) < PRESENCE_TIMEOUT_SECS * 1_000_000
}

fn color_for(identity_hex: &str) -> Color {
    let h = identity_hex
        .bytes()
        .fold(7u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    Color::color_from_hsv((h % 360) as f32, 0.7, 0.9)
}

extern "C" fn on_frame(arg: *mut c_void) {
    let state = unsafe { &mut *(arg as *mut State) };
    frame(state);
}

fn frame(state: &mut State) {
    let t = now_secs();
    let mouse = state.rl.get_mouse_position();

    if t - state.last_send >= SEND_INTERVAL {
        state.last_send = t;
        let body = format!("[{}, {}]", mouse.x, mouse.y);
        let resp = http_post("/call/set_pos", &body, "application/json", &state.token);
        if !resp.token.is_empty() {
            state.token = Some(resp.token);
        }
        if !resp.identity.is_empty() {
            state.my_identity = Some(resp.identity);
        }
    }

    if t - state.last_poll >= POLL_INTERVAL {
        state.last_poll = t;
        let resp = http_post("/sql", "SELECT * FROM user", "text/plain", &state.token);
        if !resp.body.is_empty() {
            state.users = parse_users(&resp.body);
        }
    }

    let now_micros = now_micros_since_unix_epoch();
    let mut d = state.rl.begin_drawing(&state.thread);
    d.clear_background(Color::RAYWHITE);
    for user in state
        .users
        .iter()
        .filter(|u| is_present(u.last_seen_micros, now_micros))
    {
        let center = Vector2::new(user.x, user.y);
        d.draw_poly(center, 6, HEX_RADIUS, 0.0, color_for(&user.identity_hex));
        if Some(&user.identity_hex) == state.my_identity.as_ref() {
            d.draw_poly_lines_ex(center, 6, HEX_RADIUS, 0.0, 3.0, Color::BLACK);
        }
    }
    d.draw_text("hex + merge — PoC (web)", 10, 10, 20, Color::DARKGRAY);
    d.draw_fps(640, 10);
}

fn main() {
    let (rl, thread) = raylib::init()
        .size(720, 720) // jam hard constraint
        .title("hexmerge — raylib 6.0 + SpacetimeDB (web)")
        .build();

    let state = Box::new(State {
        rl,
        thread,
        token: None,
        my_identity: None,
        users: Vec::new(),
        last_send: -SEND_INTERVAL,
        last_poll: -POLL_INTERVAL,
    });
    let arg = Box::into_raw(state) as *mut c_void;
    unsafe {
        emscripten_set_main_loop_arg(on_frame, arg, 0, true);
    }
}
