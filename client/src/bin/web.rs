//! Web (wasm32-unknown-emscripten) client — top-down 2D POC.
//!
//! Movement: ZQSD, in a single dark room (1200x1200) bigger than the
//! 720x720 window, followed by a `Camera2D` clamped to the room's bounds.
//! Without the flashlight the player only sees a faint halo around
//! themselves; holding F extends vision into a fading cone pointed in the
//! last direction moved. Six triangles of random (possibly repeated)
//! colors are scattered in the room and picked up by walking over them —
//! no flashlight required. Opening the merge table (E) shows an
//! incomplete hexagon; dragging carried triangles from the inventory onto
//! their matching-colored slot fills the hexagon, unlocking the door on
//! the right so the player can walk through and escape. Triangles are
//! generated and collected purely client-side (no server sync — see
//! WORK.md if that changes).
//!
//! Lighting is a lightmap: an offscreen `RenderTexture2D` is cleared to a
//! near-black ambient color, the halo/cone/door-glow are drawn into it
//! additively using the *same* camera as the world, then that texture is
//! blitted over the fully-drawn scene with `BLEND_MULTIPLIED` — darkness
//! masks what's already there rather than the renderer deciding what to
//! draw. (Render textures come out vertically flipped, hence the
//! negative-height source rect when sampling it back.)
//!
//! Multiplayer positions still ride the same `user` table / `set_pos`
//! reducer as before, just carrying world coordinates instead of mouse
//! coordinates — the network layer below is unchanged.
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

use raylib::prelude::*;
use serde::Deserialize;
use std::collections::HashMap;
use std::ffi::{c_void, CStr, CString};
use std::os::raw::c_char;

const WINDOW_SIZE: f32 = 720.0; // jam hard constraint
const PLAYER_RADIUS: f32 = 16.0;
const MOVE_SPEED: f32 = 220.0;

/// The one room (world space, camera-followed — bigger than the window).
const ROOM: Rect = Rect { x: 0.0, y: 0.0, w: 1200.0, h: 1200.0 };
const SPAWN_X: f32 = 600.0;
const SPAWN_Y: f32 = 600.0;

/// Door opening in the right wall. Overlaps `ROOM` by more than the
/// player's 32px diameter (mirrors the old corridor-overlap trick) so
/// walking through never gets stuck at the seam.
const DOOR_WIDTH: f32 = 100.0;
const DOOR_DEPTH: f32 = 120.0;
const DOOR_OVERLAP: f32 = 40.0;
const DOOR: Rect = Rect {
    x: ROOM.w - DOOR_OVERLAP,
    y: (ROOM.h - DOOR_WIDTH) / 2.0,
    w: DOOR_DEPTH,
    h: DOOR_WIDTH,
};
/// Player x-position past which they're considered through the door.
const ESCAPE_X: f32 = ROOM.w + 60.0;

const TRIANGLE_COUNT: usize = 6;
const TRIANGLE_RADIUS: f32 = 16.0;
/// Margin kept between triangle spawn points and the room's walls.
const TRIANGLE_MARGIN: f32 = 60.0;
/// Minimum distance kept between triangles and from the player's spawn.
const TRIANGLE_MIN_SEPARATION: f32 = 130.0;

/// Faint vision radius even without the flashlight.
const AMBIENT_GLOW_RADIUS: f32 = 90.0;
/// Flashlight cone reach — well short of the room's size, so it never
/// lights the whole map.
const FLASHLIGHT_RANGE: f32 = 300.0;
const CONE_HALF_ANGLE_DEG: f32 = 28.0;
const CONE_SEGMENTS: i32 = 16;
/// Stacked-sector fade: N concentric sectors at the same low alpha —
/// centre accumulates every layer, the rim only the outermost one.
const CONE_FADE_STEPS: usize = 10;
const CONE_LAYER_ALPHA: u8 = 16;
/// Small always-on beacon glow once the door is unlocked.
const DOOR_GLOW_RADIUS: f32 = 150.0;

const PANEL_W: f32 = 520.0;
const PANEL_H: f32 = 440.0;
const HEX_RADIUS: f32 = 110.0;
const INV_ITEM_RADIUS: f32 = 20.0;
const INV_HIT_RADIUS: f32 = 26.0;
const INV_SPACING: f32 = 56.0;

/// Max rate at which we push our own position (each send commits a
/// transaction that's broadcast to every subscriber, so keep it sane).
const SEND_INTERVAL: f64 = 1.0 / 30.0;
/// Send set_pos at least this often even when idle so last_seen stays
/// fresh — guards against ghost players when a socket dies without a
/// clean close (phone lock, wifi drop).
const HEARTBEAT_INTERVAL: f64 = 1.0;
/// Mirrors the server's presence window over user.last_seen.
const PRESENCE_TIMEOUT_SECS: i64 = 3;
/// Bounds on the interpolation window measured from real update arrival
/// gaps: floor keeps near-simultaneous updates from snapping, ceiling
/// keeps a player who idled (heartbeats only) from smearing their first
/// move across a full second.
const MIN_LERP_WINDOW: f64 = 0.02;
const MAX_LERP_WINDOW: f64 = 0.25;

#[derive(Clone, Copy)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Rect {
    /// Whether a circle of the given radius, centered at `p`, fits
    /// entirely within this rect.
    fn contains_circle(&self, p: Vector2, radius: f32) -> bool {
        p.x - radius >= self.x
            && p.x + radius <= self.x + self.w
            && p.y - radius >= self.y
            && p.y + radius <= self.y + self.h
    }
}

/// Tiny xorshift32 PRNG — just for scattering triangles client-side, no
/// need to pull in the `rand` crate for this.
struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Self {
        Self(if seed == 0 { 0x9e3779b9 } else { seed })
    }

    fn next_u32(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }

    fn next_f32(&mut self) -> f32 {
        (self.next_u32() as f64 / u32::MAX as f64) as f32
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + self.next_f32() * (hi - lo)
    }
}

/// (display label, color) palette triangles are drawn from.
const PALETTE: [(&str, Color); 6] = [
    ("red", Color::RED),
    ("orange", Color::ORANGE),
    ("green", Color::LIME),
    ("blue", Color::SKYBLUE),
    ("purple", Color::PURPLE),
    ("yellow", Color::GOLD),
];

struct Triangle {
    pos: Vector2,
    palette_index: usize,
}

/// Scatters `TRIANGLE_COUNT` triangles with independently-random colors
/// (duplicates allowed) via rejection sampling, kept apart from each
/// other and from the player's spawn point.
fn spawn_triangles(rng: &mut Rng) -> Vec<Triangle> {
    let mut triangles: Vec<Triangle> = Vec::with_capacity(TRIANGLE_COUNT);
    while triangles.len() < TRIANGLE_COUNT {
        let candidate = Vector2::new(
            rng.range(ROOM.x + TRIANGLE_MARGIN, ROOM.x + ROOM.w - TRIANGLE_MARGIN),
            rng.range(ROOM.y + TRIANGLE_MARGIN, ROOM.y + ROOM.h - TRIANGLE_MARGIN),
        );
        let far_from_spawn =
            distance(candidate, Vector2::new(SPAWN_X, SPAWN_Y)) >= TRIANGLE_MIN_SEPARATION;
        let far_from_others = triangles
            .iter()
            .all(|t| distance(t.pos, candidate) >= TRIANGLE_MIN_SEPARATION);
        if far_from_spawn && far_from_others {
            triangles.push(Triangle {
                pos: candidate,
                palette_index: (rng.next_u32() as usize) % PALETTE.len(),
            });
        }
    }
    triangles
}

/// Fisher-Yates shuffle so the merge table's target layout is a
/// permutation of the colors actually spawned (always completable, even
/// with duplicate colors).
fn shuffle6(rng: &mut Rng, arr: &mut [usize; 6]) {
    for i in (1..arr.len()).rev() {
        let j = (rng.next_u32() as usize) % (i + 1);
        arr.swap(i, j);
    }
}

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
    /// Offscreen lightmap: cleared near-black each frame, lit additively
    /// by the halo/cone/door-glow, then multiplied over the drawn scene.
    lightmap: RenderTexture2D,
    /// Our identity (lowercase hex, no 0x), from the IdentityToken message.
    my_identity: Option<String>,
    players: HashMap<String, PlayerVisual>,
    ws_status: String,
    now_micros: i64,
    last_send: f64,
    last_sent_pos: Option<(f32, f32)>,
    pos: Vector2,
    /// Last non-zero movement direction (normalized) — the flashlight
    /// cone points here, and it holds steady once the player stops.
    facing: Vector2,
    flashlight_on: bool,
    triangles: Vec<Triangle>,
    /// Palette indices of triangles carried but not yet placed.
    inventory: Vec<usize>,
    /// target_slots[i] = palette index required at hexagon slot i.
    target_slots: [usize; 6],
    placed: [bool; 6],
    merge_open: bool,
    /// Index into `inventory` currently being dragged, if any.
    dragging: Option<usize>,
    door_unlocked: bool,
    escaped: bool,
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

fn distance(a: Vector2, b: Vector2) -> f32 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

/// Moves `pos` by `delta`, one axis at a time (so sliding along a wall
/// works), rejecting any axis whose result would leave the room (and, once
/// the door is unlocked, the door opening too).
fn move_with_collision(pos: Vector2, delta: Vector2, door_unlocked: bool) -> Vector2 {
    let mut p = pos;
    let stepped_x = Vector2::new(p.x + delta.x, p.y);
    if ROOM.contains_circle(stepped_x, PLAYER_RADIUS)
        || (door_unlocked && DOOR.contains_circle(stepped_x, PLAYER_RADIUS))
    {
        p.x = stepped_x.x;
    }
    let stepped_y = Vector2::new(p.x, p.y + delta.y);
    if ROOM.contains_circle(stepped_y, PLAYER_RADIUS)
        || (door_unlocked && DOOR.contains_circle(stepped_y, PLAYER_RADIUS))
    {
        p.y = stepped_y.y;
    }
    p
}

/// Camera centered on the player, clamped so the viewport never shows
/// anything outside the room.
fn camera_for(pos: Vector2) -> Camera2D {
    let half = WINDOW_SIZE / 2.0;
    let cam_x = pos.x.clamp(ROOM.x + half, ROOM.x + ROOM.w - half);
    let cam_y = pos.y.clamp(ROOM.y + half, ROOM.y + ROOM.h - half);
    Camera2D {
        offset: Vector2::new(half, half),
        target: Vector2::new(cam_x, cam_y),
        rotation: 0.0,
        zoom: 1.0,
    }
}

fn with_alpha(c: Color, a: u8) -> Color {
    Color::new(c.r, c.g, c.b, a)
}

/// Faint always-on vision — the "we see a little without the flashlight" halo.
fn draw_ambient_glow<D: RaylibDraw>(d: &mut D, pos: Vector2) {
    d.draw_circle_gradient(
        pos.x as i32,
        pos.y as i32,
        AMBIENT_GLOW_RADIUS,
        Color::new(255, 244, 200, 90),
        Color::new(255, 244, 200, 0),
    );
}

/// Flashlight cone pointed at `facing`, faded via stacked concentric
/// sectors: the centre accumulates every layer, the rim only the last.
fn draw_light_cone<D: RaylibDraw>(d: &mut D, pos: Vector2, facing: Vector2) {
    let center_deg = facing.y.atan2(facing.x).to_degrees();
    let start = center_deg - CONE_HALF_ANGLE_DEG;
    let end = center_deg + CONE_HALF_ANGLE_DEG;
    let light = Color::new(255, 244, 200, CONE_LAYER_ALPHA);
    for step in 1..=CONE_FADE_STEPS {
        let r = FLASHLIGHT_RANGE * step as f32 / CONE_FADE_STEPS as f32;
        d.draw_circle_sector(pos, r, start, end, CONE_SEGMENTS, light);
    }
}

/// Small beacon glow marking the door once it's unlocked.
fn draw_door_glow<D: RaylibDraw>(d: &mut D) {
    let center = Vector2::new(DOOR.x + DOOR.w / 2.0, DOOR.y + DOOR.h / 2.0);
    d.draw_circle_gradient(
        center.x as i32,
        center.y as i32,
        DOOR_GLOW_RADIUS,
        Color::new(140, 255, 170, 70),
        Color::new(140, 255, 170, 0),
    );
}

fn panel_rect() -> Rectangle {
    Rectangle::new((WINDOW_SIZE - PANEL_W) / 2.0, (WINDOW_SIZE - PANEL_H) / 2.0, PANEL_W, PANEL_H)
}

fn hex_center() -> Vector2 {
    let panel = panel_rect();
    Vector2::new(panel.x + PANEL_W / 2.0, panel.y + 190.0)
}

/// The three points (apex + two rim points) of hexagon slot `i`, used both
/// for drawing that wedge and for drag-and-drop hit-testing. Returned
/// counter-clockwise (raylib's `DrawTriangle` culls the fill otherwise).
fn hex_slot_points(i: usize) -> (Vector2, Vector2, Vector2) {
    let hc = hex_center();
    let a0 = (i as f32 * 60.0 - 90.0).to_radians();
    let a1 = ((i as f32 + 1.0) * 60.0 - 90.0).to_radians();
    let p1 = Vector2::new(hc.x + HEX_RADIUS * a1.cos(), hc.y + HEX_RADIUS * a1.sin());
    let p2 = Vector2::new(hc.x + HEX_RADIUS * a0.cos(), hc.y + HEX_RADIUS * a0.sin());
    (hc, p1, p2)
}

fn inventory_slot_pos(i: usize, count: usize) -> Vector2 {
    let panel = panel_rect();
    let y = panel.y + PANEL_H - 55.0;
    let total_w = INV_SPACING * (count.max(1) as f32 - 1.0);
    let start_x = panel.x + PANEL_W / 2.0 - total_w / 2.0;
    Vector2::new(start_x + i as f32 * INV_SPACING, y)
}

/// Sign/cross-product point-in-triangle test, used to hit-test which
/// hexagon wedge a dropped triangle landed on.
fn point_in_triangle(p: Vector2, a: Vector2, b: Vector2, c: Vector2) -> bool {
    fn sign(p1: Vector2, p2: Vector2, p3: Vector2) -> f32 {
        (p1.x - p3.x) * (p2.y - p3.y) - (p2.x - p3.x) * (p1.y - p3.y)
    }
    let d1 = sign(p, a, b);
    let d2 = sign(p, b, c);
    let d3 = sign(p, c, a);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

/// Mouse-driven drag & drop between the inventory row and the hexagon
/// slots — only called while the merge table is open.
fn handle_merge_input(state: &mut State) {
    let mouse = state.rl.get_mouse_position();

    if state.rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT) {
        for i in 0..state.inventory.len() {
            let p = inventory_slot_pos(i, state.inventory.len());
            if distance(mouse, p) <= INV_HIT_RADIUS {
                state.dragging = Some(i);
                break;
            }
        }
    }

    if state.rl.is_mouse_button_released(MouseButton::MOUSE_BUTTON_LEFT) {
        if let Some(idx) = state.dragging.take() {
            if idx < state.inventory.len() {
                let palette_index = state.inventory[idx];
                for slot in 0..6 {
                    if state.placed[slot] {
                        continue;
                    }
                    let (a, b, c) = hex_slot_points(slot);
                    if state.target_slots[slot] == palette_index && point_in_triangle(mouse, a, b, c) {
                        state.placed[slot] = true;
                        state.inventory.remove(idx);
                        break;
                    }
                }
            }
        }
    }
}

extern "C" fn on_frame(arg: *mut c_void) {
    let state = unsafe { &mut *(arg as *mut State) };
    frame(state);
}

fn frame(state: &mut State) {
    let t = now_secs();
    let dt = state.rl.get_frame_time();

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

    if !state.escaped && state.rl.is_key_pressed(KeyboardKey::KEY_E) {
        state.merge_open = !state.merge_open;
        state.dragging = None;
    }

    if state.merge_open {
        handle_merge_input(state);
    } else if !state.escaped {
        // --- Input: ZQSD movement (also updates facing) + flashlight toggle ---
        let mut dir = Vector2::new(0.0, 0.0);
        if state.rl.is_key_down(KeyboardKey::KEY_Z) {
            dir.y -= 1.0;
        }
        if state.rl.is_key_down(KeyboardKey::KEY_S) {
            dir.y += 1.0;
        }
        if state.rl.is_key_down(KeyboardKey::KEY_Q) {
            dir.x -= 1.0;
        }
        if state.rl.is_key_down(KeyboardKey::KEY_D) {
            dir.x += 1.0;
        }
        if dir.x != 0.0 || dir.y != 0.0 {
            let len = (dir.x * dir.x + dir.y * dir.y).sqrt();
            let norm = Vector2::new(dir.x / len, dir.y / len);
            state.facing = norm;
            let delta = Vector2::new(norm.x * MOVE_SPEED * dt, norm.y * MOVE_SPEED * dt);
            state.pos = move_with_collision(state.pos, delta, state.door_unlocked);
        }
        if state.rl.is_key_pressed(KeyboardKey::KEY_F) {
            state.flashlight_on = !state.flashlight_on;
        }

        // Pickup: walking over a triangle grabs it, no flashlight needed.
        let pos = state.pos;
        let mut i = 0;
        while i < state.triangles.len() {
            if distance(pos, state.triangles[i].pos) <= PLAYER_RADIUS + TRIANGLE_RADIUS {
                let idx = state.triangles[i].palette_index;
                state.inventory.push(idx);
                state.triangles.swap_remove(i);
            } else {
                i += 1;
            }
        }

        if state.door_unlocked && state.pos.x >= ESCAPE_X {
            state.escaped = true;
        }
    }

    state.door_unlocked = state.placed.iter().all(|&p| p);

    // --- Network send: throttled position updates + idle heartbeat ---
    if state.my_identity.is_some() {
        let moved = state
            .last_sent_pos
            .is_none_or(|(x, y)| (x - state.pos.x).abs() > 0.5 || (y - state.pos.y).abs() > 0.5);
        let due_move = moved && t - state.last_send >= SEND_INTERVAL;
        let due_heartbeat = t - state.last_send >= HEARTBEAT_INTERVAL;
        if due_move || due_heartbeat {
            state.last_send = t;
            state.last_sent_pos = Some((state.pos.x, state.pos.y));
            run_js(&format!(
                "window.stdb && window.stdb.callReducer('set_pos', '[{}, {}]')",
                state.pos.x, state.pos.y
            ));
        }
    }

    render(state, t);
}

fn render_merge_panel(
    d: &mut RaylibDrawHandle,
    inventory: &[usize],
    placed: &[bool; 6],
    target_slots: &[usize; 6],
    dragging: Option<usize>,
    mouse_pos: Vector2,
) {
    let panel = panel_rect();
    d.draw_rectangle_rec(panel, Color::new(15, 15, 20, 235));
    d.draw_rectangle_lines_ex(panel, 2.0, Color::new(90, 90, 100, 255));
    d.draw_text("Table de merge", panel.x as i32 + 16, panel.y as i32 + 14, 20, Color::RAYWHITE);
    d.draw_text(
        "Glissez un triangle sur son emplacement",
        panel.x as i32 + 16,
        panel.y as i32 + 38,
        14,
        Color::LIGHTGRAY,
    );

    let dragged_palette = dragging.and_then(|i| inventory.get(i).copied());

    for slot in 0..6 {
        let (hc, p1, p2) = hex_slot_points(slot);
        let (_, color) = PALETTE[target_slots[slot]];
        let fill = if placed[slot] { color } else { with_alpha(color, 60) };
        d.draw_triangle(hc, p1, p2, fill);

        let hovered = dragged_palette.is_some() && !placed[slot] && point_in_triangle(mouse_pos, hc, p1, p2);
        let outline = if !hovered {
            Color::new(90, 90, 100, 255)
        } else if dragged_palette == Some(target_slots[slot]) {
            Color::WHITE
        } else {
            Color::RED
        };
        d.draw_triangle_lines(hc, p1, p2, outline);
    }

    if placed.iter().all(|&p| p) {
        d.draw_text(
            "Hexagone complet -- porte deverrouillee",
            panel.x as i32 + 16,
            panel.y as i32 + 310,
            18,
            Color::LIME,
        );
    }

    for (i, &idx) in inventory.iter().enumerate() {
        if dragging == Some(i) {
            continue;
        }
        let p = inventory_slot_pos(i, inventory.len());
        let (_, color) = PALETTE[idx];
        d.draw_poly(p, 3, INV_ITEM_RADIUS, 0.0, color);
    }

    if let Some(idx) = dragging.and_then(|i| inventory.get(i).copied()) {
        let (_, color) = PALETTE[idx];
        d.draw_poly(mouse_pos, 3, INV_ITEM_RADIUS, 0.0, color);
    }
}

fn render(state: &mut State, t: f64) {
    let now_micros = state.now_micros;
    let my_identity = state.my_identity.clone();
    let local_pos = state.pos;
    let facing = state.facing;
    let flashlight_on = state.flashlight_on;
    let door_unlocked = state.door_unlocked;
    let merge_open = state.merge_open;
    let escaped = state.escaped;
    let mouse_pos = state.rl.get_mouse_position();
    let camera = camera_for(local_pos);

    // --- Lightmap pass: ambient halo + flashlight cone + door beacon,
    // drawn additively in world space using the same camera as below. ---
    {
        let mut tm = state.rl.begin_texture_mode(&state.thread, &mut state.lightmap);
        tm.clear_background(Color::new(10, 10, 14, 255));
        {
            let mut m2 = tm.begin_mode2D(camera);
            let mut b = m2.begin_blend_mode(BlendMode::BLEND_ADDITIVE);
            draw_ambient_glow(&mut b, local_pos);
            if flashlight_on {
                draw_light_cone(&mut b, local_pos, facing);
            }
            if door_unlocked {
                draw_door_glow(&mut b);
            }
        }
    }

    let mut d = state.rl.begin_drawing(&state.thread);
    d.clear_background(Color::new(18, 18, 20, 255));

    // --- World, in camera space ---
    {
        let mut m2 = d.begin_mode2D(camera);

        m2.draw_rectangle_rec(Rectangle::new(ROOM.x, ROOM.y, ROOM.w, ROOM.h), Color::new(28, 26, 30, 255));
        m2.draw_rectangle_lines_ex(Rectangle::new(ROOM.x, ROOM.y, ROOM.w, ROOM.h), 4.0, Color::new(50, 48, 55, 255));

        let door_rect = Rectangle::new(DOOR.x, DOOR.y, DOOR.w, DOOR.h);
        if door_unlocked {
            m2.draw_rectangle_rec(door_rect, Color::new(20, 60, 30, 255));
            m2.draw_rectangle_lines_ex(door_rect, 3.0, Color::new(120, 255, 160, 255));
        } else {
            m2.draw_rectangle_rec(door_rect, Color::new(50, 15, 15, 255));
            m2.draw_rectangle_lines_ex(door_rect, 3.0, Color::new(150, 40, 40, 255));
            for i in 0..3 {
                let y = DOOR.y + DOOR.h * (i as f32 + 1.0) / 4.0 - 2.0;
                m2.draw_rectangle_rec(Rectangle::new(DOOR.x, y, DOOR.w, 4.0), Color::new(90, 30, 30, 255));
            }
        }

        for tri in &state.triangles {
            let (_, color) = PALETTE[tri.palette_index];
            m2.draw_poly(tri.pos, 3, TRIANGLE_RADIUS, 0.0, color);
        }

        for (identity_hex, player) in state
            .players
            .iter()
            .filter(|(_, p)| is_present(p.last_seen_micros, now_micros))
            .filter(|(id, _)| Some(id.as_str()) != my_identity.as_deref())
        {
            let center = player.render_pos(t);
            m2.draw_circle(center.x as i32, center.y as i32, PLAYER_RADIUS, color_for(identity_hex));
        }

        // Local player — always visible to themselves.
        m2.draw_circle(local_pos.x as i32, local_pos.y as i32, PLAYER_RADIUS, Color::WHITE);
        m2.draw_circle_lines(local_pos.x as i32, local_pos.y as i32, PLAYER_RADIUS, Color::BLACK);
    }

    // --- Darkness: multiply the lightmap over the fully-drawn scene ---
    {
        let mut b = d.begin_blend_mode(BlendMode::BLEND_MULTIPLIED);
        b.draw_texture_rec(
            state.lightmap.texture(),
            Rectangle::new(0.0, 0.0, WINDOW_SIZE, -WINDOW_SIZE),
            Vector2::new(0.0, 0.0),
            Color::WHITE,
        );
    }

    // --- HUD ---
    d.draw_text("hexmerge -- 2D POC (web)", 10, 10, 20, Color::RAYWHITE);
    d.draw_text("ZQSD bouger . F lampe . E table de merge", 10, 33, 16, Color::LIGHTGRAY);
    d.draw_text(&format!("ws: {}", state.ws_status), 10, 52, 16, Color::LIGHTGRAY);

    let collected = TRIANGLE_COUNT - state.triangles.len();
    d.draw_text(&format!("Triangles: {}/{}", collected, TRIANGLE_COUNT), 10, 76, 18, Color::RAYWHITE);
    for (i, &idx) in state.inventory.iter().enumerate() {
        let (_, color) = PALETTE[idx];
        d.draw_poly(Vector2::new(20.0 + i as f32 * 24.0, 108.0), 3, 10.0, 0.0, color);
    }

    if merge_open {
        render_merge_panel(&mut d, &state.inventory, &state.placed, &state.target_slots, state.dragging, mouse_pos);
    }

    if escaped {
        d.draw_rectangle_rec(Rectangle::new(0.0, 0.0, WINDOW_SIZE, WINDOW_SIZE), Color::new(0, 0, 0, 160));
        let text = "Echappe !";
        let size = 48;
        let w = d.measure_text(text, size);
        d.draw_text(text, (WINDOW_SIZE as i32 - w) / 2, WINDOW_SIZE as i32 / 2 - size / 2, size, Color::RAYWHITE);
    }

    d.draw_fps(640, 10);
}

fn color_for(identity_hex: &str) -> Color {
    let h = identity_hex
        .bytes()
        .fold(7u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    Color::color_from_hsv((h % 360) as f32, 0.7, 0.9)
}

fn main() {
    let (mut rl, thread) = raylib::init()
        .size(WINDOW_SIZE as i32, WINDOW_SIZE as i32)
        .title("hexmerge — raylib 6.0 + SpacetimeDB (web)")
        .build();

    let seed = (unsafe { emscripten_get_now() } as u32) | 1;
    let mut rng = Rng::new(seed);
    let triangles = spawn_triangles(&mut rng);
    let mut target_slots: [usize; 6] = std::array::from_fn(|i| triangles[i].palette_index);
    shuffle6(&mut rng, &mut target_slots);

    let lightmap = rl
        .load_render_texture(&thread, WINDOW_SIZE as u32, WINDOW_SIZE as u32)
        .expect("failed to create lightmap render texture");

    let state = Box::new(State {
        rl,
        thread,
        lightmap,
        my_identity: None,
        players: HashMap::new(),
        ws_status: "connecting".to_string(),
        now_micros: 0,
        last_send: f64::NEG_INFINITY,
        last_sent_pos: None,
        pos: Vector2::new(SPAWN_X, SPAWN_Y),
        facing: Vector2::new(1.0, 0.0),
        flashlight_on: false,
        triangles,
        inventory: Vec::new(),
        target_slots,
        placed: [false; 6],
        merge_open: false,
        dragging: None,
        door_unlocked: false,
        escaped: false,
    });
    let arg = Box::into_raw(state) as *mut c_void;
    unsafe {
        emscripten_set_main_loop_arg(on_frame, arg, 0, true);
    }
}
