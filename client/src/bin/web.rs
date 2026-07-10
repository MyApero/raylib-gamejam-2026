//! Web (wasm32-unknown-emscripten) client.
//!
//! raylib's web target is emscripten, but spacetimedb-sdk's browser support
//! needs wasm-bindgen, which only targets wasm32-unknown-unknown — the two
//! can't live in one binary. So this binary speaks SpacetimeDB's
//! `v1.json.spacetimedb` WebSocket protocol by hand instead: a JS-side
//! socket (see client/web/game.html) subscribes to the user table once,
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

#[path = "../hexgrid.rs"]
mod hexgrid;
use hexgrid::*;

use raylib::prelude::*;
use serde::Deserialize;
use std::collections::HashMap;
use std::ffi::{c_void, CStr, CString};
use std::os::raw::c_char;

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
/// client/web/game.html.
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

struct CellRow {
    id: u32,
    color: u32,
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
    /// Painted board state, id (col<<16|row) -> 0xRRGGBB color.
    cells: HashMap<u32, u32>,
    /// Precomputed once: all (col, row, center) triples.
    grid: Vec<(u32, u32, Vector2)>,
    selected: usize,
    stroke_last: Option<u32>,
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
fn parse_user_row_str(s: &serde_json::Value) -> Option<UserRow> {
    let inner = serde_json::from_str::<serde_json::Value>(s.as_str()?).ok()?;
    parse_user_row(&inner)
}

/// Parses one cell row in either encoding (mirrors `parse_user_row`): a named
/// object `{"id":..,"col":..,"row":..,"color":..,"painted_by":..,"painted_at":..}`
/// or a positional array `[id, col, row, color, [painted_by], [painted_at]]`.
fn parse_cell_row(v: &serde_json::Value) -> Option<CellRow> {
    if let Some(obj) = v.as_object() {
        Some(CellRow {
            id: obj.get("id")?.as_u64()? as u32,
            color: obj.get("color")?.as_u64()? as u32,
        })
    } else if let Some(arr) = v.as_array() {
        Some(CellRow {
            id: arr.first()?.as_u64()? as u32,
            color: arr.get(3)?.as_u64()? as u32,
        })
    } else {
        None
    }
}

/// Rows inside table updates are JSON *strings* (double-encoded).
fn parse_cell_row_str(s: &serde_json::Value) -> Option<CellRow> {
    let inner = serde_json::from_str::<serde_json::Value>(s.as_str()?).ok()?;
    parse_cell_row(&inner)
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

/// Applies the insert/delete row sets of one DatabaseUpdate, routed by
/// `table_name` to either `players` (table `user`) or `cells` (table
/// `cell`). An updated row arrives as a delete+insert pair for the same key,
/// so deletes only evict entries that were not re-inserted in the same
/// update.
fn apply_database_update(
    players: &mut HashMap<String, PlayerVisual>,
    cells: &mut HashMap<u32, u32>,
    db_update: &serde_json::Value,
    t: f64,
) {
    let Some(tables) = db_update.get("tables").and_then(|v| v.as_array()) else {
        return;
    };
    for table in tables {
        match table.get("table_name").and_then(|n| n.as_str()) {
            Some("user") => apply_user_table_update(players, table, t),
            Some("cell") => apply_cell_table_update(cells, table),
            _ => {}
        }
    }
}

fn apply_user_table_update(players: &mut HashMap<String, PlayerVisual>, table: &serde_json::Value, t: f64) {
    let Some(updates) = table.get("updates").and_then(|v| v.as_array()) else {
        return;
    };
    let mut deleted: Vec<String> = Vec::new();
    let mut inserted: Vec<UserRow> = Vec::new();
    for update in updates {
        if let Some(deletes) = update.get("deletes").and_then(|v| v.as_array()) {
            deleted.extend(deletes.iter().filter_map(parse_user_row_str).map(|r| r.identity_hex));
        }
        if let Some(inserts) = update.get("inserts").and_then(|v| v.as_array()) {
            inserted.extend(inserts.iter().filter_map(parse_user_row_str));
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

fn apply_cell_table_update(cells: &mut HashMap<u32, u32>, table: &serde_json::Value) {
    let Some(updates) = table.get("updates").and_then(|v| v.as_array()) else {
        return;
    };
    let mut deleted: Vec<u32> = Vec::new();
    let mut inserted: Vec<CellRow> = Vec::new();
    for update in updates {
        if let Some(deletes) = update.get("deletes").and_then(|v| v.as_array()) {
            deleted.extend(deletes.iter().filter_map(parse_cell_row_str).map(|r| r.id));
        }
        if let Some(inserts) = update.get("inserts").and_then(|v| v.as_array()) {
            inserted.extend(inserts.iter().filter_map(parse_cell_row_str));
        }
    }
    for id in deleted {
        if !inserted.iter().any(|r| r.id == id) {
            cells.remove(&id);
        }
    }
    for row in inserted {
        cells.insert(row.id, row.color);
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
        state.cells.clear();
        if let Some(db_update) = initial.get("database_update") {
            apply_database_update(&mut state.players, &mut state.cells, db_update, t);
        }
    } else if let Some(tx) = msg.get("TransactionUpdate") {
        // Received for our *own* reducer calls (we're the caller).
        if let Some(db_update) = tx.get("status").and_then(|s| s.get("Committed")) {
            apply_database_update(&mut state.players, &mut state.cells, db_update, t);
        }
    } else if let Some(light) = msg.get("TransactionUpdateLight") {
        // What the server actually sends subscribers for *other* clients'
        // reducer calls (verified live) — miss this and other players
        // never move.
        if let Some(db_update) = light.get("update") {
            apply_database_update(&mut state.players, &mut state.cells, db_update, t);
        }
    }
}

fn now_secs() -> f64 {
    unsafe { emscripten_get_now() / 1000.0 }
}

fn is_present(last_seen_micros: i64, now_micros: i64) -> bool {
    now_micros.saturating_sub(last_seen_micros) < PRESENCE_TIMEOUT_SECS * 1_000_000
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

    if state.rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT) {
        if let Some(i) = palette_hit(mouse) {
            state.selected = i;
        }
    } else if state.rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT) && mouse.y < PALETTE_Y0 {
        if let Some((c, r)) = nearest_cell(mouse) {
            let id = id_of(c, r);
            if Some(id) != state.stroke_last {
                run_js(&format!(
                    "window.stdb && window.stdb.callReducer('paint_cell', '[{}, {}, {}]')",
                    c, r, PALETTE[state.selected]
                ));
                state.stroke_last = Some(id);
            }
        }
    }
    if state.rl.is_mouse_button_released(MouseButton::MOUSE_BUTTON_LEFT) {
        state.stroke_last = None;
    }

    let now_micros = state.now_micros;
    let my_identity = state.my_identity.as_deref();
    let selected = state.selected;

    let mut d = state.rl.begin_drawing(&state.thread);
    d.clear_background(Color::RAYWHITE);

    for &(col, row, center) in &state.grid {
        let fill = state
            .cells
            .get(&id_of(col, row))
            .map_or(color_u32(BOARD_FILL), |&c| color_u32(c));
        d.draw_poly(center, 6, HEX_SIZE, 0.0, fill);
        d.draw_poly_lines_ex(center, 6, HEX_SIZE, 0.0, 1.0, color_u32(GRID_LINE));
    }

    for (identity_hex, player) in state
        .players
        .iter()
        .filter(|(_, p)| is_present(p.last_seen_micros, now_micros))
        .filter(|(id, _)| Some(id.as_str()) != my_identity)
    {
        let m = player.render_pos(t);
        draw_cursor(&mut d, m, color_for_hex(identity_hex));
    }

    draw_palette(&mut d, selected);

    d.draw_text("hex pixel-war (web)", 10, 10, 20, Color::DARKGRAY);
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
        .title("hex pixel-war — raylib 6.0 + SpacetimeDB (web)")
        .build();

    let state = Box::new(State {
        rl,
        thread,
        my_identity: None,
        players: HashMap::new(),
        cells: HashMap::new(),
        grid: hexgrid::cells(),
        selected: 1,
        stroke_last: None,
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
