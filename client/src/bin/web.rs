//! Web (wasm32-unknown-emscripten) client.
//!
//! raylib's web target is emscripten, but spacetimedb-sdk's browser support
//! needs wasm-bindgen, which only targets wasm32-unknown-unknown — the two
//! can't live in one binary. So this binary speaks SpacetimeDB's
//! `v1.json.spacetimedb` WebSocket protocol by hand instead: a JS-side
//! socket (see client/web/index.html) subscribes to the user table once,
//! the server *pushes* a TransactionUpdate on every commit, and each frame
//! we drain those pushed messages from a JS mailbox via
//! emscripten_run_script_string — no polling, no blocking the render loop.
//! Reducer calls (set_pos) go out over the same socket.
//!
//! Auth: the WebSocket route accepts the token as a `?token=` query param
//! (browsers can't set headers on a WebSocket), and the server pushes an
//! IdentityToken message first thing on every connection, so no separate
//! HTTP identity bootstrap is needed. (If you're ever tempted to read
//! spacetime-identity-token response *headers* over HTTP instead: the
//! server's CORS layer doesn't expose them to browser JS — that's a trap.)
//!
//! Wire-format quirks, verified against a live 2.7 server rather than the
//! docs: rows inside InitialSubscription are named JSON objects, but rows
//! inside transaction updates are positional arrays (parse_user_row
//! handles both); and subscribers receive other clients' commits as
//! TransactionUpdateLight, only their own as full TransactionUpdate.

use raylib::prelude::*;
use serde::Deserialize;
use std::collections::HashMap;
use std::ffi::{c_void, CStr, CString};
use std::os::raw::c_char;

const HEX_RADIUS: f32 = 28.0;
/// Max rate at which we push our own cursor position (each send commits a
/// transaction that's broadcast to every subscriber, so keep it sane).
const SEND_INTERVAL: f64 = 1.0 / 30.0;
/// Send set_pos at least this often even when idle so last_seen stays
/// fresh — guards against ghost hexagons when a socket dies without a
/// clean close (phone lock, wifi drop).
const HEARTBEAT_INTERVAL: f64 = 1.0;
/// Mirrors the native client's presence window over user.last_seen.
const PRESENCE_TIMEOUT_SECS: i64 = 3;
/// Bounds on the interpolation window measured from real update arrival
/// gaps: floor keeps near-simultaneous updates from snapping, ceiling
/// keeps a player who idled (heartbeats only) from smearing their first
/// move across a full second.
const MIN_LERP_WINDOW: f64 = 0.02;
const MAX_LERP_WINDOW: f64 = 0.25;

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

/// Everything the JS side hands us once per frame — see stdb.frame() in
/// client/web/index.html.
#[derive(Deserialize, Default)]
struct FrameData {
    #[serde(default)]
    status: String,
    #[serde(default)]
    identity: Option<String>,
    #[serde(default)]
    now_micros: i64,
    #[serde(default)]
    msgs: Vec<String>,
}

struct UserRow {
    identity_hex: String,
    x: f32,
    y: f32,
    last_seen_micros: i64,
}

/// Other players' positions arrive as discrete pushes (at whatever rate
/// that player sends), which reads as choppy motion if drawn directly. We
/// interpolate from where we were drawing when the update landed (`prev`)
/// towards the new position (`target`) over `window` — the measured gap
/// between this player's last two updates — so rendering stays smooth at
/// any inbound rate.
struct PlayerVisual {
    prev: Vector2,
    target: Vector2,
    updated_at: f64,
    window: f64,
    last_seen_micros: i64,
}

impl PlayerVisual {
    fn render_pos(&self, now: f64) -> Vector2 {
        let t = ((now - self.updated_at) / self.window).clamp(0.0, 1.0) as f32;
        Vector2::new(
            self.prev.x + (self.target.x - self.prev.x) * t,
            self.prev.y + (self.target.y - self.prev.y) * t,
        )
    }
}

struct State {
    rl: RaylibHandle,
    thread: RaylibThread,
    /// Our identity (lowercase hex, no 0x), from the IdentityToken message.
    my_identity: Option<String>,
    players: HashMap<String, PlayerVisual>,
    ws_status: String,
    now_micros: i64,
    last_send: f64,
    last_sent_pos: Option<(f32, f32)>,
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

/// Strips the "0x" prefix and lowercases, so identities compare equal no
/// matter which encoding they arrived in.
fn normalize_identity(hex: &str) -> String {
    hex.strip_prefix("0x").unwrap_or(hex).to_lowercase()
}

/// Parses one user row in either of the two encodings the server actually
/// sends (see module docs): a named object
///   {"identity":{"__identity__":"0x.."},"name":..,"online":..,"x":..,"y":..,
///    "last_seen":{"__timestamp_micros_since_unix_epoch__":..}}
/// or a positional array matching the schema order
///   [["0x.."], name, online, x, y, [micros]].
fn parse_user_row(v: &serde_json::Value) -> Option<UserRow> {
    if let Some(obj) = v.as_object() {
        Some(UserRow {
            identity_hex: normalize_identity(obj.get("identity")?.get("__identity__")?.as_str()?),
            x: obj.get("x")?.as_f64()? as f32,
            y: obj.get("y")?.as_f64()? as f32,
            last_seen_micros: obj
                .get("last_seen")
                .and_then(|t| t.get("__timestamp_micros_since_unix_epoch__"))
                .and_then(|t| t.as_i64())
                .unwrap_or(0),
        })
    } else if let Some(arr) = v.as_array() {
        Some(UserRow {
            identity_hex: normalize_identity(arr.first()?.as_array()?.first()?.as_str()?),
            x: arr.get(3)?.as_f64()? as f32,
            y: arr.get(4)?.as_f64()? as f32,
            last_seen_micros: arr
                .get(5)
                .and_then(|t| t.as_array())
                .and_then(|a| a.first())
                .and_then(|t| t.as_i64())
                .unwrap_or(0),
        })
    } else {
        None
    }
}

/// Rows inside table updates are JSON *strings* (double-encoded).
fn parse_row_str(s: &serde_json::Value) -> Option<UserRow> {
    let inner = serde_json::from_str::<serde_json::Value>(s.as_str()?).ok()?;
    parse_user_row(&inner)
}

fn upsert(players: &mut HashMap<String, PlayerVisual>, row: UserRow, t: f64) {
    let new_target = Vector2::new(row.x, row.y);
    players
        .entry(row.identity_hex)
        .and_modify(|p| {
            p.prev = p.render_pos(t);
            p.window = (t - p.updated_at).clamp(MIN_LERP_WINDOW, MAX_LERP_WINDOW);
            p.target = new_target;
            p.updated_at = t;
            p.last_seen_micros = row.last_seen_micros;
        })
        .or_insert(PlayerVisual {
            prev: new_target,
            target: new_target,
            updated_at: t,
            window: MIN_LERP_WINDOW,
            last_seen_micros: row.last_seen_micros,
        });
}

/// Applies the insert/delete row sets of one DatabaseUpdate to `players`.
/// An updated row arrives as a delete+insert pair for the same identity,
/// so deletes only evict players that were not re-inserted in the same
/// update.
fn apply_database_update(
    players: &mut HashMap<String, PlayerVisual>,
    db_update: &serde_json::Value,
    t: f64,
) {
    let Some(tables) = db_update.get("tables").and_then(|v| v.as_array()) else {
        return;
    };
    for table in tables {
        if table.get("table_name").and_then(|n| n.as_str()) != Some("user") {
            continue;
        }
        let Some(updates) = table.get("updates").and_then(|v| v.as_array()) else {
            continue;
        };
        let mut deleted: Vec<String> = Vec::new();
        let mut inserted: Vec<UserRow> = Vec::new();
        for update in updates {
            if let Some(deletes) = update.get("deletes").and_then(|v| v.as_array()) {
                deleted.extend(deletes.iter().filter_map(parse_row_str).map(|r| r.identity_hex));
            }
            if let Some(inserts) = update.get("inserts").and_then(|v| v.as_array()) {
                inserted.extend(inserts.iter().filter_map(parse_row_str));
            }
        }
        for id in deleted {
            if !inserted.iter().any(|r| r.identity_hex == id) {
                players.remove(&id);
            }
        }
        for row in inserted {
            upsert(players, row, t);
        }
    }
}

fn handle_message(state: &mut State, raw: &str, t: f64) {
    let Ok(msg) = serde_json::from_str::<serde_json::Value>(raw) else {
        return;
    };
    if let Some(id_token) = msg.get("IdentityToken") {
        if let Some(hex) = id_token.get("identity").and_then(|i| i.get("__identity__")).and_then(|i| i.as_str()) {
            state.my_identity = Some(normalize_identity(hex));
        }
    } else if let Some(initial) = msg.get("InitialSubscription") {
        // Reconnects replay the full table; drop stale local state first.
        state.players.clear();
        if let Some(db_update) = initial.get("database_update") {
            apply_database_update(&mut state.players, db_update, t);
        }
    } else if let Some(tx) = msg.get("TransactionUpdate") {
        // Received for our *own* reducer calls (we're the caller).
        if let Some(db_update) = tx.get("status").and_then(|s| s.get("Committed")) {
            apply_database_update(&mut state.players, db_update, t);
        }
    } else if let Some(light) = msg.get("TransactionUpdateLight") {
        // What the server actually sends subscribers for *other* clients'
        // reducer calls (verified live) — miss this and other players
        // never move.
        if let Some(db_update) = light.get("update") {
            apply_database_update(&mut state.players, db_update, t);
        }
    }
}

fn now_secs() -> f64 {
    unsafe { emscripten_get_now() / 1000.0 }
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

    // One JS call pulls in everything the socket received since last frame.
    let raw = run_js("window.stdb ? window.stdb.frame() : '{}'");
    let data: FrameData = serde_json::from_str(&raw).unwrap_or_default();
    state.ws_status = data.status;
    state.now_micros = data.now_micros;
    if state.my_identity.is_none() {
        state.my_identity = data.identity.as_deref().map(normalize_identity);
    }
    for msg in &data.msgs {
        handle_message(state, msg, t);
    }

    let mouse = state.rl.get_mouse_position();

    if state.my_identity.is_some() {
        let moved = state
            .last_sent_pos
            .is_none_or(|(x, y)| (x - mouse.x).abs() > 0.5 || (y - mouse.y).abs() > 0.5);
        let due_move = moved && t - state.last_send >= SEND_INTERVAL;
        let due_heartbeat = t - state.last_send >= HEARTBEAT_INTERVAL;
        if due_move || due_heartbeat {
            state.last_send = t;
            state.last_sent_pos = Some((mouse.x, mouse.y));
            run_js(&format!(
                "window.stdb && window.stdb.callReducer('set_pos', '[{}, {}]')",
                mouse.x, mouse.y
            ));
        }
    }

    let now_micros = state.now_micros;
    let my_identity = state.my_identity.as_deref();

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
    // Drawn from the live mouse position rather than the subscribed row,
    // so our own hexagon tracks the cursor with zero round-trip delay.
    if let Some(id) = my_identity {
        let center = Vector2::new(mouse.x, mouse.y);
        d.draw_poly(center, 6, HEX_RADIUS, 0.0, color_for(id));
        d.draw_poly_lines_ex(center, 6, HEX_RADIUS, 0.0, 3.0, Color::BLACK);
    }
    d.draw_text("hex + merge — PoC (web)", 10, 10, 20, Color::DARKGRAY);
    d.draw_text(
        &format!("ws: {}", state.ws_status),
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
        my_identity: None,
        players: HashMap::new(),
        ws_status: "connecting".to_string(),
        now_micros: 0,
        last_send: f64::NEG_INFINITY,
        last_sent_pos: None,
    });
    let arg = Box::into_raw(state) as *mut c_void;
    unsafe {
        emscripten_set_main_loop_arg(on_frame, arg, 0, true);
    }
}
