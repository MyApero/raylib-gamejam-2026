//! Web (wasm32-unknown-emscripten) client.
//!
//! raylib's web target is emscripten, but spacetimedb-sdk's browser support
//! needs wasm-bindgen, which only targets wasm32-unknown-unknown — the two
//! can't live in one binary. So instead of a live WebSocket subscription,
//! this binary polls SpacetimeDB's plain HTTP API (POST /call, POST /sql)
//! via a synchronous XMLHttpRequest injected through
//! emscripten_run_script_string. See WORK.md.
//!
//! Identity/token bootstrap uses POST /v1/identity, whose response body
//! carries {identity, token} — deliberately not the spacetime-identity /
//! spacetime-identity-token response *headers* that /call and /sql also
//! return, because SpacetimeDB's CORS layer doesn't call
//! `.expose_headers(...)`, so browsers hide those headers from JS on
//! cross-origin requests (curl, which ignores CORS entirely, will still
//! show them — that's a trap). The bootstrapped token is then reused as a
//! Bearer token on every subsequent request so the server sees one stable
//! identity instead of minting a new one per call.

use raylib::prelude::*;
use serde::Deserialize;
use std::collections::HashMap;
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
const POLL_INTERVAL: f64 = 0.15;
/// Retry the identity bootstrap this often until it succeeds.
const BOOTSTRAP_RETRY_INTERVAL: f64 = 1.0;
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
    body: String,
}

#[derive(Deserialize)]
struct IdentityResponse {
    identity: String,
    token: String,
}

#[derive(Clone)]
struct UserRow {
    identity_hex: String,
    x: f32,
    y: f32,
    last_seen_micros: i64,
}

/// Other players only get a fresh position every POLL_INTERVAL over the
/// network, which reads as choppy motion if drawn directly. We interpolate
/// from the position we were at when the last poll landed (`prev`) towards
/// the newly polled one (`target`) over the following POLL_INTERVAL, so
/// rendering stays smooth every frame regardless of the real poll rate.
struct PlayerVisual {
    prev: Vector2,
    target: Vector2,
    updated_at: f64,
    last_seen_micros: i64,
}

impl PlayerVisual {
    fn render_pos(&self, now: f64) -> Vector2 {
        let t = ((now - self.updated_at) / POLL_INTERVAL).clamp(0.0, 1.0) as f32;
        Vector2::new(
            self.prev.x + (self.target.x - self.prev.x) * t,
            self.prev.y + (self.target.y - self.prev.y) * t,
        )
    }
}

struct State {
    rl: RaylibHandle,
    thread: RaylibThread,
    /// None until the identity bootstrap succeeds; nothing else can happen
    /// without it (every /call and /sql needs the Bearer token).
    identity: Option<(String, String)>,
    players: HashMap<String, PlayerVisual>,
    last_send: f64,
    last_poll: f64,
    last_bootstrap_attempt: f64,
    last_poll_latency_ms: f64,
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
fn http_post(url: &str, body: &str, content_type: &str, token: Option<&str>) -> HttpResponse {
    let js_url = serde_json::to_string(url).unwrap_or_default();
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
                return JSON.stringify({{ body: xhr.responseText }});
            }} catch (e) {{
                return JSON.stringify({{ body: '' }});
            }}
        }})()"#
    );
    let raw = run_js(&script);
    serde_json::from_str(&raw).unwrap_or_default()
}

fn database_url(path: &str) -> String {
    format!("{HOST}/v1/database/{DB_NAME}{path}")
}

/// Strips the "0x" prefix /sql rows use, to match the plain hex string
/// POST /v1/identity returns — the two endpoints format identities
/// differently even though they're the same underlying bytes.
fn normalize_identity(hex: &str) -> String {
    hex.strip_prefix("0x").unwrap_or(hex).to_lowercase()
}

fn bootstrap_identity() -> Option<(String, String)> {
    let resp = http_post(&format!("{HOST}/v1/identity"), "", "application/json", None);
    let parsed: IdentityResponse = serde_json::from_str(&resp.body).ok()?;
    Some((normalize_identity(&parsed.identity), parsed.token))
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
            .map(normalize_identity)
            .unwrap_or_default();
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
    run_js("Date.now() * 1000").parse::<i64>().unwrap_or(0)
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

    if state.identity.is_none() && t - state.last_bootstrap_attempt >= BOOTSTRAP_RETRY_INTERVAL {
        state.last_bootstrap_attempt = t;
        state.identity = bootstrap_identity();
    }

    let mouse = state.rl.get_mouse_position();

    if let Some((_, token)) = &state.identity {
        let token = token.clone();
        if t - state.last_send >= SEND_INTERVAL {
            state.last_send = t;
            let body = format!("[{}, {}]", mouse.x, mouse.y);
            http_post(
                &database_url("/call/set_pos"),
                &body,
                "application/json",
                Some(&token),
            );
        }

        if t - state.last_poll >= POLL_INTERVAL {
            state.last_poll = t;
            let call_start = now_secs();
            let resp = http_post(
                &database_url("/sql"),
                "SELECT * FROM user",
                "text/plain",
                Some(&token),
            );
            state.last_poll_latency_ms = (now_secs() - call_start) * 1000.0;
            if !resp.body.is_empty() {
                for row in parse_users(&resp.body) {
                    let new_target = Vector2::new(row.x, row.y);
                    let prev = state
                        .players
                        .get(&row.identity_hex)
                        .map(|p| p.render_pos(t))
                        .unwrap_or(new_target);
                    state.players.insert(
                        row.identity_hex.clone(),
                        PlayerVisual {
                            prev,
                            target: new_target,
                            updated_at: t,
                            last_seen_micros: row.last_seen_micros,
                        },
                    );
                }
            }
        }
    }

    let now_micros = now_micros_since_unix_epoch();
    let my_identity = state.identity.as_ref().map(|(id, _)| id.as_str());

    let mut d = state.rl.begin_drawing(&state.thread);
    d.clear_background(Color::RAYWHITE);
    for (identity_hex, player) in state
        .players
        .iter()
        .filter(|(_, p)| is_present(p.last_seen_micros, now_micros))
        .filter(|(id, _)| Some(id.as_str()) != my_identity)
    {
        let center = player.render_pos(t);
        d.draw_poly(center, 6, HEX_RADIUS, 0.0, color_for(identity_hex));
    }
    // Drawn from the live mouse position rather than the polled table, so
    // our own hexagon tracks the cursor every frame instead of snapping
    // once per POLL_INTERVAL like everyone else's.
    if let Some(id) = my_identity {
        let center = Vector2::new(mouse.x, mouse.y);
        d.draw_poly(center, 6, HEX_RADIUS, 0.0, color_for(id));
        d.draw_poly_lines_ex(center, 6, HEX_RADIUS, 0.0, 3.0, Color::BLACK);
    }
    d.draw_text("hex + merge — PoC (web)", 10, 10, 20, Color::DARKGRAY);
    d.draw_text(
        &format!("poll latency: {:.0}ms", state.last_poll_latency_ms),
        10,
        35,
        16,
        Color::GRAY,
    );
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
        identity: None,
        players: HashMap::new(),
        last_send: -SEND_INTERVAL,
        last_poll: -POLL_INTERVAL,
        last_bootstrap_attempt: -BOOTSTRAP_RETRY_INTERVAL,
        last_poll_latency_ms: 0.0,
    });
    let arg = Box::into_raw(state) as *mut c_void;
    unsafe {
        emscripten_set_main_loop_arg(on_frame, arg, 0, true);
    }
}
