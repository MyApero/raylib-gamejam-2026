//! Web (wasm32-unknown-emscripten) client.
//!
//! raylib's web target is emscripten, but spacetimedb-sdk's browser support
//! needs wasm-bindgen, which only targets wasm32-unknown-unknown — the two
//! can't live in one binary. So this binary speaks SpacetimeDB's
//! `v1.json.spacetimedb` WebSocket protocol by hand instead: a JS-side
//! socket (see client/web/game.html) subscribes to every table once, the
//! server *pushes* a TransactionUpdate on every commit, and each frame we
//! drain those pushed messages from a JS mailbox via
//! emscripten_run_script_string — no polling, no blocking the render loop.
//! Reducer calls go out over the same socket.
//!
//! Everything else — geometry, HSV, camera math, the HUD widgets — is
//! shared with the native client via `world.rs`/`ui.rs` (`#[path]`-included
//! below); this file only supplies the SpacetimeDB row plumbing, the input
//! loop and touch handling, in place of `main.rs`'s SDK-backed `DbConnection`.
//!
//! Auth: the WebSocket route accepts the token as a `?token=` query param
//! (browsers can't set headers on a WebSocket), and the server pushes an
//! IdentityToken message first thing on every connection, so no separate
//! HTTP identity bootstrap is needed.
//!
//! Wire-format quirks, verified live against the local 2.6.1 instance
//! (see probe notes in status.md) rather than the docs:
//! - Rows inside `InitialSubscription` and inside transaction updates are
//!   BOTH JSON *strings* (double-encoded) holding either a named object
//!   (`InitialSubscription`) or a positional array (transaction updates).
//! - `Identity` encodes as `{"__identity__": "0x.."}` when named, or as a
//!   nested one-element array `["0x.."]` when positional.
//! - `Timestamp` encodes as `{"__timestamp_micros_since_unix_epoch__": N}`
//!   when named, or `[N]` when positional.
//! - `Option<T>` encodes the SAME way in BOTH the named and positional
//!   forms: a two-element array `[tag, payload]`, tag `0` = `Some(payload)`,
//!   tag `1` = `None` (payload is `{}`/`[]`, ignored).
//! - Subscribers receive other clients' commits as `TransactionUpdateLight`,
//!   only their own as full `TransactionUpdate`.

#[path = "../replay.rs"]
mod replay;
#[path = "../sfx.rs"]
mod sfx;
#[path = "../title_map.rs"]
mod title_map;
#[path = "../ui.rs"]
mod ui;
#[path = "../world.rs"]
mod world;
use world::constants::*;

use raylib::prelude::*;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString, c_void};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::os::raw::c_char;
use std::time::{Duration, Instant};

const RECOVERED_HISTORY_PATH: &str = "/hexel-tile-history.bin";
// Version 3: a kind-5 (island insert) record's `color` field now carries the
// island's resolved border color as of that point in history (owner's
// `border_color` pin, else their seed hue) instead of always 0 — see
// `tools/history-extractor`'s matching `HISTORY_MAGIC` bump for why replay
// needs this baked in rather than resolved from live tables. Version 4 drops
// the leading transaction offset, which this parser never read: at eight of
// twenty-five bytes it was most of what limited how much history could ship.
const RECOVERED_HISTORY_MAGIC: &[u8] = b"HEXELHIST\x04";
const RECOVERED_HISTORY_RECORD_BYTES: u64 = 17;
// Mirrors `tools/history-extractor`'s `UNKNOWN_BORDER_COLOR` — never a valid
// packed HSV value, so it unambiguously means "the extractor couldn't
// resolve this island's border color at that point in history".
const UNKNOWN_BORDER_COLOR: u32 = u32::MAX;

unsafe extern "C" {
    fn emscripten_run_script_string(script: *const c_char) -> *const c_char;
    fn emscripten_set_main_loop_arg(
        func: extern "C" fn(*mut c_void),
        arg: *mut c_void,
        fps: i32,
        simulate_infinite_loop: bool,
    );
    fn hexel_gif_begin(width: i32, height: i32) -> i32;
    fn hexel_gif_frame(rgba: *mut u8, delay_cs: i32, pitch: i32);
    fn hexel_gif_end(length: *mut usize) -> *mut u8;
    fn hexel_gif_free(data: *mut c_void);
}

const GIF_EXPORT_SIZE: i32 = 240;
const GIF_CAPTURE_EVERY_FRAMES: u32 = 6; // 10 FPS at the game's 60 FPS target
const GIF_FRAME_DELAY_CS: i32 = 10;
/// Frames a GIF samples per second of real time (every
/// `GIF_CAPTURE_EVERY_FRAMES`th frame at the 60 FPS target).
const GIF_CAPTURE_FPS: f32 = 60.0 / GIF_CAPTURE_EVERY_FRAMES as f32;
/// Rough bytes per pixel a GIF frame compresses to for this game's flat,
/// few-colour pixel art, and the rough VP9 bitrate MediaRecorder settles on
/// for the 720x720 canvas. Both feed the size figures shown before recording
/// starts — deliberately approximate, and labelled as estimates in the UI.
const GIF_BYTES_PER_PIXEL: f32 = 0.18;
const WEBM_BITS_PER_SEC: f32 = 2_500_000.0;
/// How long the Download button confirms with "Downloaded" before offering
/// another capture. Long enough to read, short enough not to look stuck.
const DOWNLOADED_BADGE_SECS: f32 = 2.0;

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
    /// Set once by `window.stdb.frame()` the
    /// frame after a trial import connection (see `importToken` in
    /// game.html) gets rejected — read-once, same as `msgs`, so a stale
    /// error can't linger and pop up again on some unrelated later frame.
    #[serde(default)]
    import_error: Option<String>,
    /// The tab became visible again since the last frame. Read-once, like
    /// `msgs`. The frame loop is throttled or stopped while hidden, so a
    /// timed UI state (the "Downloaded" badge) would otherwise resume its
    /// countdown mid-way on return instead of being over with.
    #[serde(default)]
    page_returned: bool,
    /// A fresher title bake has been written to the emscripten filesystem and
    /// is ready to swap in. Read-once, like `msgs`.
    #[serde(default)]
    title_map_ready: bool,
}

#[derive(Clone)]
struct UserRow {
    name: Option<String>,
    online: bool,
    cx: f32,
    cy: f32,
    hue: u16,
    sat: u8,
    val: u8,
    locked: bool,
    xp: u64,
}

struct InventoryRow {
    owner_hex: String,
    hue: u16,
    /// Needed (unlike everywhere else that's skipped `obtained_at` —
    /// see `GiftRow`'s comment) to join a `None`-`obtained_with_hex` row
    /// against a `hexa_event` row stamped with the identical `ctx.timestamp`
    /// — see `apply_hexa`'s server-side comment on why this is a timestamp
    /// join rather than a new bool field.
    obtained_at_micros: i64,
    obtained_with_hex: Option<String>,
    /// Mirrors `main.rs`'s `Inventory.from_gift` — a flying-gift hue
    /// grant, distinct from `reset_account`'s reseed even though both leave
    /// `obtained_with_hex: None`.
    from_gift: bool,
}

/// Mirrors `main.rs`'s `Gift` binding — one row = the single currently-
/// active flying-gift pickup. `expires_at` isn't read client-side (the row
/// simply disappears once the server's tick expires it), so it's skipped
/// here, same as `parse_inventory` already skips `obtained_at`.
struct GiftRow {
    x: f32,
    y: f32,
    spawned_at_micros: i64,
}

/// Mirrors `main.rs`'s `HexaEvent` binding, trimmed to the one field
/// this client actually reads — the inventory-toast join described on
/// `InventoryRow::obtained_at_micros` (the ignited-hexagon flash is driven
/// by `HexaClusterRow.ignited` instead, so `cx`/`cy`/`member_count` aren't
/// needed here).
struct HexaEventRow {
    at_micros: i64,
}

/// Mirrors the merge-event fields needed for raw-HSL recent colors and the
/// pre-merge toast equation.
struct MergeEventRow {
    a_hex: String,
    b_hex: String,
    hue_a: u16,
    hue_b: u16,
    merged_hue: u16,
    merged_sat: u8,
    merged_val: u8,
    at_micros: i64,
}

/// Mirrors `main.rs`'s `HexaCluster` binding — one row per currently-
/// clustered user, server-authoritative (see the server-side table's doc
/// comment for why: every client renders the exact same group instead of
/// each guessing its own approximate clustering).
struct HexaClusterRow {
    member_count: u32,
    vertex_index: u32,
    ignited: bool,
}

struct IslandRow {
    owner_hex: String,
    slot: u32,
    likes: u32,
    itch_rate_id: Option<u32>,
    created_at_micros: i64,
    border_color: Option<u32>,
    border_hidden: bool,
}

struct IslandCellRow {
    island_id: u32,
    q: i32,
    r: i32,
    color: u32,
    painted_at_micros: i64,
}

struct MarginCellRow {
    q: i32,
    r: i32,
    color: u32,
    painted_at_micros: i64,
}

/// One row per (island, liker) — see the server-side comment on
/// `IslandLike` for why uniqueness is enforced in the reducer, not here.
struct IslandLikeRow {
    island_id: u32,
    liker_hex: String,
}

/// `next_rerank_at` drives the countdown banner. `admin_hex` (author
/// request, admin "draw anywhere") is consumed by `is_admin` below; `frozen`
/// still has no client behavior.
struct ConfigRow {
    next_rerank_at_micros: Option<i64>,
    admin_hex: Option<String>,
}

/// Delegates to shared `world::normalize_identity_hex` (also left-pads to the
/// full 64-hex-char `Identity` width — the wire's minimal-hex encoding of
/// `Identity::ZERO` is `"0x0"`, not 64 zeros, which used to make every
/// zero-identity comparison silently fail on web).
fn normalize_identity(hex: &str) -> String {
    world::normalize_identity_hex(hex)
}

fn short_hex(id: &str) -> &str {
    &id[..8.min(id.len())]
}

/// A table row as delivered over the wire is either a named JSON object
/// (InitialSubscription) or a positional JSON array matching schema field
/// order (transaction updates) — this lets every `parse_*` function read a
/// field by name OR index without caring which encoding it got.
enum RowView<'a> {
    Obj(&'a serde_json::Map<String, Value>),
    Arr(&'a Vec<Value>),
}

impl<'a> RowView<'a> {
    fn field(&self, name: &str, index: usize) -> Option<&'a Value> {
        match self {
            RowView::Obj(m) => m.get(name),
            RowView::Arr(a) => a.get(index),
        }
    }
}

fn row_view(v: &Value) -> Option<RowView<'_>> {
    if let Some(m) = v.as_object() {
        Some(RowView::Obj(m))
    } else if let Some(a) = v.as_array() {
        Some(RowView::Arr(a))
    } else {
        None
    }
}

/// `Identity`: named `{"__identity__": "0x.."}` or positional `["0x.."]`.
fn identity_hex(v: &Value) -> Option<String> {
    if let Some(s) = v.get("__identity__").and_then(|s| s.as_str()) {
        return Some(normalize_identity(s));
    }
    v.as_array()?.first()?.as_str().map(normalize_identity)
}

/// `Timestamp`: named `{"__timestamp_micros_since_unix_epoch__": N}` or
/// positional `[N]`.
fn timestamp_micros(v: &Value) -> i64 {
    v.get("__timestamp_micros_since_unix_epoch__")
        .and_then(|t| t.as_i64())
        .or_else(|| v.as_array()?.first()?.as_i64())
        .unwrap_or(0)
}

/// `Option<T>`: `[0, payload]` = Some(payload), `[1, _]` = None — same shape
/// whether the row itself is named or positional.
fn opt_value(v: &Value) -> Option<&Value> {
    let arr = v.as_array()?;
    if arr.first()?.as_u64()? == 0 {
        arr.get(1)
    } else {
        None
    }
}

fn parse_user(v: &Value) -> Option<(String, UserRow)> {
    let r = row_view(v)?;
    let identity = identity_hex(r.field("identity", 0)?)?;
    Some((
        identity,
        UserRow {
            name: r
                .field("name", 1)?
                .pipe(opt_value)
                .and_then(|s| s.as_str())
                .map(String::from),
            online: r.field("online", 2)?.as_bool()?,
            cx: r.field("cx", 3)?.as_f64()? as f32,
            cy: r.field("cy", 4)?.as_f64()? as f32,
            hue: r.field("hue", 6)?.as_u64()? as u16,
            sat: r.field("sat", 7)?.as_u64()? as u8,
            val: r.field("val", 8)?.as_u64()? as u8,
            locked: r.field("locked", 9)?.as_bool()?,
            xp: r.field("xp", 10)?.as_u64()?,
        },
    ))
}

fn parse_inventory(v: &Value) -> Option<(u64, InventoryRow)> {
    let r = row_view(v)?;
    let id = r.field("id", 0)?.as_u64()?;
    Some((
        id,
        InventoryRow {
            owner_hex: identity_hex(r.field("owner", 1)?)?,
            hue: r.field("hue", 2)?.as_u64()? as u16,
            obtained_at_micros: timestamp_micros(r.field("obtained_at", 3)?),
            obtained_with_hex: r
                .field("obtained_with", 4)?
                .pipe(opt_value)
                .and_then(identity_hex),
            from_gift: r.field("from_gift", 5)?.as_bool()?,
        },
    ))
}

fn parse_island(v: &Value) -> Option<(u32, IslandRow)> {
    let r = row_view(v)?;
    let id = r.field("id", 0)?.as_u64()? as u32;
    Some((
        id,
        IslandRow {
            owner_hex: identity_hex(r.field("owner", 1)?)?,
            slot: r.field("slot", 2)?.as_u64()? as u32,
            likes: r.field("likes", 3)?.as_u64()? as u32,
            itch_rate_id: r
                .field("itch_rate_id", 4)?
                .pipe(opt_value)
                .and_then(|v| v.as_u64())
                .map(|n| n as u32),
            created_at_micros: timestamp_micros(r.field("created_at", 5)?),
            border_color: r
                .field("border_color", 6)?
                .pipe(opt_value)
                .and_then(|v| v.as_u64())
                .map(|n| n as u32),
            border_hidden: r.field("border_hidden", 7)?.as_bool()?,
        },
    ))
}

fn parse_island_like(v: &Value) -> Option<(u64, IslandLikeRow)> {
    let r = row_view(v)?;
    let id = r.field("id", 0)?.as_u64()?;
    Some((
        id,
        IslandLikeRow {
            island_id: r.field("island_id", 1)?.as_u64()? as u32,
            liker_hex: identity_hex(r.field("liker", 2)?)?,
        },
    ))
}

fn parse_config(v: &Value) -> Option<(u32, ConfigRow)> {
    let r = row_view(v)?;
    let id = r.field("id", 0)?.as_u64()? as u32;
    Some((
        id,
        ConfigRow {
            next_rerank_at_micros: r
                .field("next_rerank_at", 3)?
                .pipe(opt_value)
                .map(timestamp_micros),
            admin_hex: r.field("admin", 2)?.pipe(opt_value).and_then(identity_hex),
        },
    ))
}

fn parse_island_cell(v: &Value) -> Option<(u32, IslandCellRow)> {
    let r = row_view(v)?;
    let id = r.field("id", 0)?.as_u64()? as u32;
    Some((
        id,
        IslandCellRow {
            island_id: r.field("island_id", 1)?.as_u64()? as u32,
            q: r.field("q", 2)?.as_i64()? as i32,
            r: r.field("r", 3)?.as_i64()? as i32,
            color: r.field("color", 4)?.as_u64()? as u32,
            painted_at_micros: timestamp_micros(r.field("painted_at", 6)?),
        },
    ))
}

fn parse_margin_cell(v: &Value) -> Option<(u32, MarginCellRow)> {
    let r = row_view(v)?;
    let id = r.field("id", 0)?.as_u64()? as u32;
    Some((
        id,
        MarginCellRow {
            q: r.field("q", 1)?.as_i64()? as i32,
            r: r.field("r", 2)?.as_i64()? as i32,
            color: r.field("color", 3)?.as_u64()? as u32,
            painted_at_micros: timestamp_micros(r.field("painted_at", 5)?),
        },
    ))
}

fn parse_gift(v: &Value) -> Option<(u64, GiftRow)> {
    let r = row_view(v)?;
    let id = r.field("id", 0)?.as_u64()?;
    Some((
        id,
        GiftRow {
            x: r.field("x", 1)?.as_f64()? as f32,
            y: r.field("y", 2)?.as_f64()? as f32,
            spawned_at_micros: timestamp_micros(r.field("spawned_at", 3)?),
        },
    ))
}

fn parse_hexa_event(v: &Value) -> Option<(u64, HexaEventRow)> {
    let r = row_view(v)?;
    let id = r.field("id", 0)?.as_u64()?;
    Some((
        id,
        HexaEventRow {
            at_micros: timestamp_micros(r.field("at", 1)?),
        },
    ))
}

/// Mirrors `main.rs`'s `MergeEvent` binding. Field order
/// matches the server struct (`id, at, a, b, hue_a, hue_b, merged_hue,
/// merged_sat, merged_val`).
fn parse_merge_event(v: &Value) -> Option<(u64, MergeEventRow)> {
    let r = row_view(v)?;
    let id = r.field("id", 0)?.as_u64()?;
    Some((
        id,
        MergeEventRow {
            at_micros: timestamp_micros(r.field("at", 1)?),
            a_hex: identity_hex(r.field("a", 2)?)?,
            b_hex: identity_hex(r.field("b", 3)?)?,
            hue_a: r.field("hue_a", 4)?.as_u64()? as u16,
            hue_b: r.field("hue_b", 5)?.as_u64()? as u16,
            merged_hue: r.field("merged_hue", 6)?.as_u64()? as u16,
            merged_sat: r.field("merged_sat", 7)?.as_u64()? as u8,
            merged_val: r.field("merged_val", 8)?.as_u64()? as u8,
        },
    ))
}

fn parse_hexa_cluster(v: &Value) -> Option<(String, HexaClusterRow)> {
    let r = row_view(v)?;
    let identity = identity_hex(r.field("identity", 0)?)?;
    // Compatible server fields `cx`/`cy` remain at positions 2/3 but are
    // now always the world origin, so the client no longer stores them.
    Some((
        identity,
        HexaClusterRow {
            member_count: r.field("member_count", 4)?.as_u64()? as u32,
            vertex_index: r.field("vertex_index", 5)?.as_u64()? as u32,
            ignited: r.field("ignited", 6)?.as_bool()?,
        },
    ))
}

/// Small pipe-forward helper so the `Option<&Value>` chains above (field ->
/// unwrap tag -> read payload) read left to right instead of nesting.
trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl<T> Pipe for T {}

/// Rows inside `updates[].inserts`/`deletes` are JSON *strings*
/// (double-encoded), in both `InitialSubscription` and transaction updates.
fn parse_row_str<K, T>(s: &Value, parse: impl Fn(&Value) -> Option<(K, T)>) -> Option<(K, T)> {
    let inner = serde_json::from_str::<Value>(s.as_str()?).ok()?;
    parse(&inner)
}

struct Tables {
    users: HashMap<String, UserRow>,
    inventory: HashMap<u64, InventoryRow>,
    islands: HashMap<u32, IslandRow>,
    island_cells: HashMap<u32, IslandCellRow>,
    margin_cells: HashMap<u32, MarginCellRow>,
    island_likes: HashMap<u64, IslandLikeRow>,
    configs: HashMap<u32, ConfigRow>,
    gifts: HashMap<u64, GiftRow>,
    hexa_events: HashMap<u64, HexaEventRow>,
    hexa_clusters: HashMap<String, HexaClusterRow>,
    merge_events: HashMap<u64, MergeEventRow>,
}

impl Tables {
    fn new() -> Self {
        Self {
            users: HashMap::new(),
            inventory: HashMap::new(),
            islands: HashMap::new(),
            island_cells: HashMap::new(),
            margin_cells: HashMap::new(),
            island_likes: HashMap::new(),
            configs: HashMap::new(),
            gifts: HashMap::new(),
            hexa_events: HashMap::new(),
            hexa_clusters: HashMap::new(),
            merge_events: HashMap::new(),
        }
    }

    fn clear(&mut self) {
        self.users.clear();
        self.inventory.clear();
        self.islands.clear();
        self.island_cells.clear();
        self.margin_cells.clear();
        self.island_likes.clear();
        self.configs.clear();
        self.gifts.clear();
        self.hexa_events.clear();
        self.hexa_clusters.clear();
        self.merge_events.clear();
    }

    fn apply(&mut self, db_update: &Value) {
        let Some(tables) = db_update.get("tables").and_then(|v| v.as_array()) else {
            return;
        };
        for table in tables {
            let Some(name) = table.get("table_name").and_then(|n| n.as_str()) else {
                continue;
            };
            let Some(updates) = table.get("updates").and_then(|v| v.as_array()) else {
                continue;
            };
            match name {
                "user" => apply_updates(&mut self.users, updates, parse_user),
                "inventory" => apply_updates(&mut self.inventory, updates, parse_inventory),
                "island" => apply_updates(&mut self.islands, updates, parse_island),
                "island_cell" => apply_updates(&mut self.island_cells, updates, parse_island_cell),
                "margin_cell" => apply_updates(&mut self.margin_cells, updates, parse_margin_cell),
                "island_like" => apply_updates(&mut self.island_likes, updates, parse_island_like),
                "config" => apply_updates(&mut self.configs, updates, parse_config),
                "gift" => apply_updates(&mut self.gifts, updates, parse_gift),
                "hexa_event" => apply_updates(&mut self.hexa_events, updates, parse_hexa_event),
                "hexa_cluster" => {
                    apply_updates(&mut self.hexa_clusters, updates, parse_hexa_cluster)
                }
                "merge_event" => apply_updates(&mut self.merge_events, updates, parse_merge_event),
                _ => {}
            }
        }
    }
}

/// Generic insert/delete application for one table's `updates` array. An
/// updated row arrives as a delete+insert pair for the same key, so this
/// only needs to apply deletes then inserts, in that order, per update
/// entry (never skips a delete just because *some* row got inserted this
/// batch — only if the SAME key was re-inserted).
fn apply_updates<K: std::hash::Hash + Eq, T>(
    map: &mut HashMap<K, T>,
    updates: &[Value],
    parse: impl Fn(&Value) -> Option<(K, T)>,
) {
    for update in updates {
        if let Some(deletes) = update.get("deletes").and_then(|v| v.as_array()) {
            for d in deletes {
                if let Some((k, _)) = parse_row_str(d, &parse) {
                    map.remove(&k);
                }
            }
        }
        if let Some(inserts) = update.get("inserts").and_then(|v| v.as_array()) {
            for i in inserts {
                if let Some((k, row)) = parse_row_str(i, &parse) {
                    map.insert(k, row);
                }
            }
        }
    }
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

/// Calls a reducer with JSON-encoded args, safely embedded as JS string
/// literals regardless of content (arbitrary player-typed text in
/// `set_name` included) — both the reducer name and the args-array JSON are
/// passed through `serde_json::to_string` before being spliced into the JS
/// source `run_js` hands to `eval`, so quotes/backslashes/newlines in a
/// player name can't break out of the literal or inject script.
fn call_reducer(name: &str, args: Value) {
    let js_name = serde_json::to_string(name).unwrap();
    let js_args = serde_json::to_string(&args.to_string()).unwrap();
    run_js(&format!(
        "window.stdb && window.stdb.callReducer({js_name}, {js_args})"
    ));
}

/// What a hovered world cell is paintable as, from `me`'s point of view —
/// mirrors `main.rs`'s `Paintable` against this file's plain `Tables`
/// instead of `ctx.db`.
enum Paintable {
    OwnIsland(i32, i32),
    Margin(i32, i32),
    /// The slot-0 community island — paintable by anyone.
    Community(i32, i32),
    /// Author request (admin "draw anywhere"): absolute world coords on
    /// someone else's island, paintable only because `me` is admin.
    Anywhere(i32, i32),
    None,
}

fn my_island<'a>(tables: &'a Tables, me: &str) -> Option<(&'a IslandRow, u32)> {
    tables
        .islands
        .iter()
        .find(|(_, isl)| isl.owner_hex == me)
        .map(|(&id, isl)| (isl, id))
}

/// Author request (admin "draw anywhere"): true once `me` has claimed the
/// admin role via `claim_admin` — mirrors `main.rs`'s `is_admin` against this
/// file's plain `Tables` instead of `ctx.db`.
fn is_admin(tables: &Tables, me: &str) -> bool {
    tables
        .configs
        .get(&0)
        .is_some_and(|c| c.admin_hex.as_deref() == Some(me))
}

fn island_world_center(island: &IslandRow) -> Vector2 {
    let (q, r) = world::slot_coords(island.slot);
    let (cx, cy) = world::slot_center(q, r);
    world::axial_to_world(cx, cy)
}

fn classify(tables: &Tables, me: &str, world_q: i32, world_r: i32) -> Paintable {
    if let Some((island, _)) = my_island(tables, me) {
        let (q, r) = world::slot_coords(island.slot);
        let (ccx, ccy) = world::slot_center(q, r);
        let (lq, lr) = (world_q - ccx, world_r - ccy);
        if world::hexdist(lq, lr) <= ISLAND_RADIUS {
            return Paintable::OwnIsland(lq, lr);
        }
    }
    // Slot 0 is always the origin (fixed geometry, never a real
    // player's island), so no island lookup needed.
    let (q0, r0) = world::slot_coords(0);
    let (ccx0, ccy0) = world::slot_center(q0, r0);
    let (lq0, lr0) = (world_q - ccx0, world_r - ccy0);
    if world::hexdist(lq0, lr0) <= ISLAND_RADIUS {
        return Paintable::Community(lq0, lr0);
    }
    if !world::in_any_island_territory(world_q, world_r) {
        return Paintable::Margin(world_q, world_r);
    }
    if is_admin(tables, me) {
        return Paintable::Anywhere(world_q, world_r);
    }
    Paintable::None
}

/// Like `classify`, but considers every island — long-press merge can
/// target any player's painted tile.
fn island_at(tables: &Tables, world_q: i32, world_r: i32) -> Option<(u32, i32, i32)> {
    tables.islands.iter().find_map(|(&id, island)| {
        let (q, r) = world::slot_coords(island.slot);
        let (ccx, ccy) = world::slot_center(q, r);
        let (lq, lr) = (world_q - ccx, world_r - ccy);
        (world::hexdist(lq, lr) <= ISLAND_RADIUS).then_some((id, lq, lr))
    })
}

/// The painted island cell (if any) at absolute world axial `(q, r)`
/// (`kind` 0, matching `merge_with_cell`'s wire signature), with its row id
/// and current hue. Margin tiles are deliberately never a target — see the
/// server-side comment on `merge_with_cell` (no color discovery from the
/// margin; it's everyone's to paint, so a tile there gets overwritten
/// mid-gesture far more often than an island tile).
fn merge_target_at(tables: &Tables, world_q: i32, world_r: i32) -> Option<(u8, u32, u16)> {
    let (island_id, lq, lr) = island_at(tables, world_q, world_r)?;
    tables
        .island_cells
        .iter()
        .find(|(_, c)| c.island_id == island_id && c.q == lq && c.r == lr)
        .map(|(&id, c)| (0u8, id, world::unpack_hsv(c.color).0))
}

/// Mirrors `main.rs`'s
/// `painted_color_at` — the color at world axial `(q, r)`, any island's cell
/// or a margin cell, whichever it is.
fn painted_color_at(tables: &Tables, world_q: i32, world_r: i32) -> Option<(u16, u8, u8)> {
    if let Some((island_id, lq, lr)) = island_at(tables, world_q, world_r) {
        return tables
            .island_cells
            .values()
            .find(|c| c.island_id == island_id && c.q == lq && c.r == lr)
            .map(|c| world::unpack_hsv(c.color));
    }
    tables
        .margin_cells
        .values()
        .find(|c| c.q == world_q && c.r == world_r)
        .map(|c| world::unpack_hsv(c.color))
}

/// Mirrors `main.rs`'s `world_fit` — a camera
/// pose framing every currently-known island's center.
fn world_fit(tables: &Tables, fallback: Vector2, fallback_zoom: f32) -> (Vector2, f32) {
    world_fit_with_min_zoom(tables, fallback, fallback_zoom, 0.25)
}

fn world_fit_with_min_zoom(
    tables: &Tables,
    fallback: Vector2,
    fallback_zoom: f32,
    min_zoom: f32,
) -> (Vector2, f32) {
    let mut min = Vector2::new(f32::MAX, f32::MAX);
    let mut max = Vector2::new(f32::MIN, f32::MIN);
    let mut any = false;
    for island in tables.islands.values() {
        let center = island_world_center(island);
        any = true;
        min.x = min.x.min(center.x);
        min.y = min.y.min(center.y);
        max.x = max.x.max(center.x);
        max.y = max.y.max(center.y);
    }
    if !any {
        return (fallback, fallback_zoom);
    }
    let target = Vector2::new((min.x + max.x) / 2.0, (min.y + max.y) / 2.0);
    let span = (max.x - min.x).max(max.y - min.y) + ISLAND_RADIUS as f32 * 4.0;
    let zoom = (620.0 / span.max(1.0)).clamp(min_zoom, ISLAND_FIT_ZOOM);
    (target, zoom)
}

/// Display label for a merge partner in the "new color" toast: their name if
/// set (every player gets a random one at first connect, server-side).
/// Falls back to a generic label, never the partner's identity — mirrors
/// `main.rs`'s `player_label`, see its comment for why.
fn player_label(tables: &Tables, id: &str) -> String {
    // Mirrors `main.rs` — the community island's sentinel
    // owner isn't a real player.
    if id == world::COMMUNITY_OWNER_HEX {
        return "Free Isle".to_string();
    }
    tables
        .users
        .get(id)
        .and_then(|u| u.name.clone().filter(|n| !n.is_empty()))
        .unwrap_or_else(|| "another player".to_string())
}

/// Whether `me` already effectively has `hue` unlocked (within
/// `HUE_TOLERANCE`) — mirrors `main.rs`'s `have_hue`.
fn have_hue(tables: &Tables, me: &str, hue: u16) -> bool {
    tables
        .inventory
        .values()
        .any(|inv| inv.owner_hex == me && world::hue_dist(inv.hue, hue) <= HUE_TOLERANCE)
}

fn pick_color_at(
    tables: &Tables,
    ui_state: &mut ui::UiState,
    me: &str,
    world_q: i32,
    world_r: i32,
) -> bool {
    let level = tables.users.get(me).map_or(0, |u| world::level_of(u.xp));
    match world::eyedropper_pick(
        painted_color_at(tables, world_q, world_r),
        |hue| have_hue(tables, me, hue),
        world::sat_cap(level),
    ) {
        world::EyedropperPick::Selected { hue, sat, val } => {
            call_reducer("set_brush", serde_json::json!([hue, sat, val]));
            ui_state.note_used_color(ui::RecentColor { hue, sat, val });
            true
        }
        world::EyedropperPick::Locked => {
            // Not an error — the long-press this arms is how you unlock it.
            ui_state.show_info_toast("not unlocked — long-press to merge".to_string());
            false
        }
        world::EyedropperPick::Empty => {
            ui_state.show_info_toast("no painted color here".to_string());
            false
        }
    }
}

/// Mirrors `main.rs`'s `format_age` exactly, just in raw micros instead
/// of `Timestamp` (this binary has no SDK time type).
fn format_age(now_micros: i64, created_at_micros: i64) -> String {
    let secs = now_micros.saturating_sub(created_at_micros) / 1_000_000;
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Whether `me` has already liked `island_id` — mirrors `main.rs`'s
/// `already_liked`, shared by the popup payload and the double-click toggle.
fn already_liked(tables: &Tables, island_id: u32, me: &str) -> bool {
    tables
        .island_likes
        .values()
        .any(|l| l.island_id == island_id && l.liker_hex == me)
}

/// Mirrors `main.rs`'s `resolve_border_color` — what an
/// island's border currently resolves to, ignoring `border_hidden` (the
/// custom pin, or the seed-hue default when unset). Feeds the "Set border to
/// current color" popup preview swatch.
fn resolve_border_color(tables: &Tables, island: &IslandRow) -> Color {
    if let Some(packed) = island.border_color {
        let (h, s, v) = world::unpack_hsv(packed);
        world::hsv_color(h, s, v)
    } else {
        let seed_hue = tables
            .inventory
            .values()
            .find(|inv| inv.owner_hex == island.owner_hex && inv.obtained_with_hex.is_none())
            .map(|inv| inv.hue);
        world::hsv_color(seed_hue.unwrap_or(0), 40, 100)
    }
}

/// Mirrors `main.rs`'s `open_island_info`, reading from the local
/// `Tables` cache instead of `ctx.db`.
fn open_island_info(state: &mut State, island_id: u32) {
    let Some(island) = state.tables.islands.get(&island_id) else {
        return;
    };
    let me = state.my_identity.clone();
    let me = me.as_deref();
    let is_own = Some(island.owner_hex.as_str()) == me;
    let owner_label = player_label(&state.tables, &island.owner_hex);
    let likes = island.likes;
    let link_id = island.itch_rate_id;
    let age_label = format_age(state.now_micros, island.created_at_micros);
    let already_liked = me.is_some_and(|me| already_liked(&state.tables, island_id, me));
    let border_hidden = island.border_hidden;
    let border_color = resolve_border_color(&state.tables, island);
    state.ui_state.open_island_info(ui::IslandInfo {
        island_id,
        owner_label,
        likes,
        age_label,
        link_id,
        is_own,
        already_liked,
        border_hidden,
        border_color,
    });
}

/// Mirrors `main.rs`'s `open_admin_edit`,
/// reading from the local `Tables` cache instead of `ctx.db`.
fn open_admin_edit(state: &mut State, island_id: u32) {
    let Some(island) = state.tables.islands.get(&island_id) else {
        return;
    };
    let Some(owner) = state.tables.users.get(&island.owner_hex) else {
        return;
    };
    let name = owner.name.clone().unwrap_or_default();
    let likes = island.likes;
    let xp = owner.xp;
    state.ui_state.open_admin_edit(island_id, &name, likes, xp);
}

/// In-flight long-press-to-merge gesture — mirrors `main.rs`'s `LongPress`.
/// Also tracks the island-info target, fired instead on a plain click —
/// see `main.rs`'s comment on the same struct.
struct LongPress {
    press_screen: Vector2,
    press_at: Instant,
    target: Option<(u8, u32, u16)>,
    info_target: Option<u32>,
    fired: bool,
}

/// Two-finger pinch/pan gesture: pins the world point that was under the
/// midpoint of the two fingers at gesture start, so both fingers moving
/// together (pan) and apart/together (pinch-zoom) fall out of the same
/// anchor rather than needing separate delta bookkeeping.
struct Pinch {
    anchor_world: Vector2,
    start_dist: f32,
    start_zoom: f32,
}

/// Sequential reader for the local commitlog recovery artifact. Keeping only
/// the current tile maps in memory lets the web replay handle millions of
/// historical mutations without loading all events into the wasm heap.
struct RecoveredReplay {
    reader: BufReader<File>,
    /// Inclusive/exclusive source-event range shown by this replay. Whole
    /// world playback uses the full file; a selected island uses only the
    /// period from its creation through its last mutation.
    playback_start: usize,
    playback_end: usize,
    /// First event that must be applied to reconstruct world state, as
    /// opposed to `playback_start`, where the *visible* replay begins.
    ///
    /// A `Cut` moves `playback_start` forward but never this: the events
    /// before the cut still built the world (islands only exist once their
    /// creation event has been applied, and every earlier paint is part of
    /// the picture), so they are replayed instantly to seed the starting
    /// frame. Dropping them instead left the canvas empty with no islands.
    seed_start: usize,
    applied_events: usize,
    /// Island cells use island-local coordinates, so their island id must be
    /// part of the key. Using just `(q, r)` collapses every player's island
    /// onto the origin.
    island_cells: HashMap<(u32, i32, i32), u32>,
    margin_cells: HashMap<(i32, i32), u32>,
    /// Current slot for every island at this point in the recovered event
    /// stream. This also follows re-ranks instead of using today's layout.
    island_slots: HashMap<u32, u32>,
    /// Each island's border color as resolved by the extractor at its most
    /// recent insert event up to this point in history (owner's
    /// `border_color` pin, else their seed hue) — never cleared on a kind-4
    /// delete, same as `island_cells`/`island_slots`: a stale entry is
    /// harmless since `display_slot` already hides anything not currently
    /// in `island_slots`, and `id` is `#[auto_inc]` so ids are never reused.
    /// This is what makes a deleted island's border still render correctly
    /// when scrubbing back to before its deletion — `state.tables.islands`
    /// (live) has nothing left to look up by then.
    island_border: HashMap<u32, u32>,
    /// Slot each island ends the recorded history in, from a scan of the
    /// whole file at open. Backs the overlay's "Islands: fixed" mode, which
    /// holds every island still instead of following its re-ranks — a
    /// re-rank moves an island's whole artwork across the map mid-replay,
    /// which is history but reads as a glitch in an exported video.
    final_slots: HashMap<u32, u32>,
    /// `None` replays the whole world; `Some(id)` retains only that island.
    island_filter: Option<u32>,
}

impl RecoveredReplay {
    fn open(island_filter: Option<u32>) -> Result<Self, String> {
        let file = File::open(RECOVERED_HISTORY_PATH)
            .map_err(|error| format!("replay history is unavailable: {error}"))?;
        let length = file
            .metadata()
            .map_err(|error| format!("could not inspect replay history: {error}"))?
            .len();
        let payload = length
            .checked_sub(RECOVERED_HISTORY_MAGIC.len() as u64)
            .ok_or_else(|| "replay history is truncated".to_string())?;
        if payload % RECOVERED_HISTORY_RECORD_BYTES != 0 {
            return Err("replay history has an invalid record length".to_string());
        }
        let mut reader = BufReader::new(file);
        let mut magic = [0; RECOVERED_HISTORY_MAGIC.len()];
        reader
            .read_exact(&mut magic)
            .map_err(|error| format!("could not read replay history: {error}"))?;
        if magic != RECOVERED_HISTORY_MAGIC {
            return Err("replay history has an unknown format".to_string());
        }
        let total_events = (payload / RECOVERED_HISTORY_RECORD_BYTES) as usize;
        // One pass over this compact fixed-width file collects both the final
        // slot layout and, for a focused replay, the target island's own
        // range — so it can start at that island's actual creation instead of
        // spending most of the world timeline on an empty screen.
        let mut scan = BufReader::new(
            File::open(RECOVERED_HISTORY_PATH)
                .map_err(|error| format!("could not scan replay history: {error}"))?,
        );
        scan.seek(SeekFrom::Start(RECOVERED_HISTORY_MAGIC.len() as u64))
            .map_err(|error| format!("could not scan replay history: {error}"))?;
        let mut bytes = [0_u8; RECOVERED_HISTORY_RECORD_BYTES as usize];
        let mut final_slots = HashMap::new();
        let mut first = None;
        let mut first_paint = None;
        let mut last_paint = 0;
        let mut last = 0;
        for index in 0..total_events {
            scan.read_exact(&mut bytes)
                .map_err(|error| format!("could not scan replay event {index}: {error}"))?;
            let kind = bytes[0];
            let island_id = u32::from_le_bytes(bytes[1..5].try_into().expect("fixed replay event"));
            if kind == 5 {
                let slot = i32::from_le_bytes(bytes[5..9].try_into().expect("fixed replay event"));
                // Left in place on a kind-4 delete: a deleted island's cells
                // still need somewhere to sit while scrubbed back to before
                // its deletion, and ids are never reused.
                final_slots.insert(island_id, slot as u32);
            }
            if matches!(kind, 0 | 1 | 4 | 5) && island_filter == Some(island_id) {
                first.get_or_insert(index);
                last = index + 1;
                if matches!(kind, 0 | 1) {
                    first_paint.get_or_insert(index);
                    last_paint = index + 1;
                }
            }
        }
        let (seed_start, playback_start, playback_end) = match island_filter {
            None => (0, 0, total_events),
            Some(target_id) => {
                let Some(first) = first else {
                    return Err(format!("island #{target_id} has no recovered history"));
                };
                // Trimmed to the island's first and last colour change. An
                // island is created well before anyone paints on it, and
                // re-ranks keep touching it after the last stroke — playing
                // those back is a still frame at each end of the replay.
                // Everything from its creation up to the first paint is still
                // applied, instantly, as the seed.
                match first_paint {
                    Some(first_paint) => (first, first_paint, last_paint),
                    None => (first, first, last),
                }
            }
        };
        let mut replay = Self {
            reader,
            playback_start,
            playback_end,
            seed_start,
            applied_events: playback_start,
            island_cells: HashMap::new(),
            margin_cells: HashMap::new(),
            island_slots: HashMap::new(),
            island_border: HashMap::new(),
            final_slots,
            island_filter,
        };
        replay.reset()?;
        Ok(replay)
    }

    fn reset(&mut self) -> Result<(), String> {
        self.reader
            .seek(SeekFrom::Start(
                RECOVERED_HISTORY_MAGIC.len() as u64
                    + self.seed_start as u64 * RECOVERED_HISTORY_RECORD_BYTES,
            ))
            .map_err(|error| format!("could not restart replay history: {error}"))?;
        self.applied_events = self.seed_start;
        self.island_cells.clear();
        self.margin_cells.clear();
        self.island_slots.clear();
        self.island_border.clear();
        // Replay everything between the seed origin and the visible window in
        // one go: it is not animated, it *is* the starting frame. Without it a
        // cut start would begin from an empty world with no islands, so
        // nothing the later events touch would render.
        self.apply_until(self.playback_start)
    }

    /// Restrict playback to a sub-range of the current one, using the same
    /// fractions the trim handles marked.
    ///
    /// Playback is paced uniformly over event *indices* (see `advance_to`),
    /// not over wall-clock time, so narrowing the clock's timestamp range
    /// alone changes nothing here — the cut has to be applied to the index
    /// range too, which is what actually drops events from the replay and
    /// from the count the overlay shows.
    fn narrow(&mut self, start_fraction: f32, end_fraction: f32) -> Result<(), String> {
        let base = self.playback_start;
        let span = self.playback_end.saturating_sub(base) as f64;
        let offset = |fraction: f32| base + (span * fraction.clamp(0.0, 1.0) as f64) as usize;
        let new_start = offset(start_fraction);
        // Always leave at least one event, so a replay can't end up empty.
        let new_end = offset(end_fraction).max(new_start + 1).min(self.playback_end);
        self.playback_start = new_start.min(new_end - 1);
        self.playback_end = new_end;
        self.reset()
    }

    fn advance_to(&mut self, progress: f32) -> Result<(), String> {
        let target = self.playback_start
            + (((self.playback_end - self.playback_start) as f64) * progress.clamp(0.0, 1.0) as f64)
                as usize;
        if target < self.applied_events {
            self.reset()?;
        }
        self.apply_until(target)
    }

    /// Apply events forward until `applied_events` reaches `target`, reading
    /// sequentially from wherever the reader currently sits. Shared by the
    /// paced playback and by `reset`'s instant seeding pass.
    fn apply_until(&mut self, target: usize) -> Result<(), String> {
        let mut bytes = [0_u8; RECOVERED_HISTORY_RECORD_BYTES as usize];
        while self.applied_events < target {
            self.reader.read_exact(&mut bytes).map_err(|error| {
                format!(
                    "could not read replay event {}: {error}",
                    self.applied_events
                )
            })?;
            let kind = bytes[0];
            // Playback is paced uniformly by event order, not by the gaps
            // between the timestamps the file was sorted on.
            let island_id =
                u32::from_le_bytes(bytes[1..5].try_into().expect("fixed replay event"));
            let q = i32::from_le_bytes(bytes[5..9].try_into().expect("fixed replay event"));
            let r = i32::from_le_bytes(bytes[9..13].try_into().expect("fixed replay event"));
            let color = u32::from_le_bytes(bytes[13..17].try_into().expect("fixed replay event"));
            let include_island = self
                .island_filter
                .is_none_or(|target_id| target_id == island_id);
            match kind {
                0 if include_island => {
                    self.island_cells.remove(&(island_id, q, r));
                }
                1 if include_island => {
                    self.island_cells.insert((island_id, q, r), color);
                }
                2 if self.island_filter.is_none() => {
                    self.margin_cells.remove(&(q, r));
                }
                3 if self.island_filter.is_none() => {
                    self.margin_cells.insert((q, r), color);
                }
                4 if include_island => {
                    self.island_slots.remove(&island_id);
                }
                5 if include_island => {
                    self.island_slots.insert(island_id, q as u32);
                    self.island_border.insert(island_id, color);
                }
                0..=5 => {}
                _ => return Err(format!("replay history has invalid event kind {kind}")),
            }
            self.applied_events += 1;
        }
        Ok(())
    }

    fn visible_tiles(&self) -> usize {
        self.island_cells.len() + self.margin_cells.len()
    }

    /// A single-island replay is an artwork export, not a historical
    /// leaderboard visualisation.  Keep that island at the canonical origin
    /// for every frame so current or historic re-ranks cannot push it out of
    /// the camera centre.  Presence still comes from `island_slots`, so an
    /// island only appears after its recovered creation event and disappears
    /// at its recovered deletion event.
    ///
    /// `motion` is the world replay's islands toggle: false pins every island
    /// to the slot it ends the history in, so the layout stays put while the
    /// painting plays back.
    fn display_slot(&self, island_id: u32, motion: bool) -> Option<u32> {
        self.island_slots.get(&island_id).map(|&historical_slot| {
            if self.island_filter == Some(island_id) {
                0
            } else if motion {
                historical_slot
            } else {
                self.final_slots
                    .get(&island_id)
                    .copied()
                    .unwrap_or(historical_slot)
            }
        })
    }

    fn playback_events(&self) -> usize {
        self.playback_end - self.playback_start
    }

    fn applied_playback_events(&self) -> usize {
        self.applied_events - self.playback_start
    }
}

struct State {
    rl: RaylibHandle,
    thread: RaylibThread,
    /// Baked world map drawn behind the title screen; `None` if the embedded
    /// PNG failed to decode, which just costs the title its backdrop.
    title_map: Option<Texture2D>,
    /// Camera pose and newest-paint stamp of whichever bake `title_map`
    /// currently holds — the compiled-in one at boot, replaced together with
    /// the texture if a fresher render is fetched (see `swap_title_map`).
    /// They have to move as a set: the pose positions the image in the world
    /// and the stamp decides which cells are drawn live on top of it, so a
    /// mismatched pair puts the map in the wrong place or double-draws.
    title_map_view: (Vector2, f32),
    title_map_baked_at: i64,
    /// The fetch is kicked off from the first frame rather than at startup:
    /// emscripten's filesystem has to exist before JS can write into it.
    title_map_requested: bool,
    my_identity: Option<String>,
    /// True only after an InitialSubscription message has been fully parsed
    /// into `tables`. SpacetimeDB applies that initial snapshot atomically;
    /// subsequent TransactionUpdates can then stream normally.
    subscription_ready: bool,
    tables: Tables,
    camera: Camera2D,
    centered_on_island: bool,
    /// Mirrors `main.rs`'s `intro_started_at`/`intro_from`.
    intro_started_at: Option<Instant>,
    intro_from: Option<(Vector2, f32)>,
    ui_state: ui::UiState,
    /// Local-only retained-world replay. `None` is ordinary gameplay;
    /// `Some` suppresses mutations and chronologically reveals timestamps.
    replay_clock: Option<replay::ReplayClock>,
    /// Full local history for web replay, loaded from the recovery artifact
    /// rather than inferred from the latest table snapshot.
    recovered_replay: Option<RecoveredReplay>,
    /// A replay the player asked for before the history had been fetched.
    /// The artifact is no longer an emscripten preload (it gated startup on
    /// a 258 MiB download — see build-web.sh), so the first replay of a
    /// session has to wait for `hexelEnsureHistory` to write it into MEMFS.
    /// Held here and retried from `frame` once the fetch reports ready.
    history_pending: Option<PendingReplay>,
    /// Browser canvas recording is active until replay reaches its end (or
    /// the viewer is closed), then JavaScript downloads a WebM file.
    replay_recording: bool,
    /// True for an export launched from the export panel, where controls and
    /// the cursor stay out of the captured timelapse frames.
    clean_timeline_export: bool,
    /// A compact, real GIF capture of the selected island or world replay.
    /// Frames are sampled after raylib finishes drawing, then encoded by
    /// `msf_gif`.
    gif_recording: bool,
    /// The `island_filter` a `gif_recording` in progress was started with
    /// (`None` = whole world) — snapshotted at start so the finish handler
    /// doesn't need `state.recovered_replay`, which a mid-recording close
    /// may already have cleared.
    gif_export_island: Option<u32>,
    gif_frame_counter: u32,
    /// Last whole percent pushed to the DOM export-progress bar, or -1 when
    /// it is hidden. Tracked so the JS bridge is only crossed when the
    /// displayed number actually changes, not on every frame of a capture.
    export_progress_shown: i32,
    /// Seconds left on the "Downloaded" confirmation. Counts down to zero,
    /// at which point the button offers Download again — a capture at a
    /// different speed or cut is a reasonable next thing to want.
    export_saved_for: f32,
    known_inventory_ids: HashSet<u64>,
    inventory_seeded: bool,
    known_merge_event_ids: HashSet<u64>,
    merge_events_seeded: bool,
    known_hexa_event_ids: HashSet<u64>,
    hexa_events_seeded: bool,
    /// Mirrors `main.rs`'s `hexa_display` — persisted, lerped hexagon-
    /// vertex snap positions (including the local player's own, per the
    /// author) keyed by identity hex string (this client has no SDK
    /// `Identity` type).
    hexa_display: HashMap<String, Vector2>,
    /// Level-up toast: mirrors `main.rs`'s `last_level` — `None` until the
    /// first frame `me` is known, so connecting already at some level
    /// doesn't fire a spurious toast.
    last_level: Option<u64>,
    /// Sound rework: mirrors `main.rs`'s `last_xp`/`pending_gift_claim`/
    /// `music_started` — see that file's comment for the delta-gating and
    /// gift-attribution logic.
    last_xp: Option<u64>,
    pending_gift_claim: Option<Instant>,
    music_started: bool,
    long_press: Option<LongPress>,
    /// Mirrors `main.rs`'s `pending_info_click` — a clean
    /// single-click on a foreign island, held pending for
    /// DOUBLE_CLICK_WINDOW before it resolves into actually opening the
    /// info popup (see the gesture block's comment for why).
    pending_info_click: Option<(Instant, Vector2, u32)>,
    /// Decision 17: mirrors `main.rs`'s `hover_target` —
    /// `(island_id, hover_started_at)` for the currently-hovered foreign
    /// island, tracked purely by cursor position, independent of
    /// `pending_info_click` above (left unchanged for double-click-to-like
    /// and touch).
    hover_target: Option<(u32, Instant)>,
    pinch: Option<Pinch>,
    /// Mirrors `main.rs`'s `middle_click`.
    middle_click: Option<Vector2>,
    stroke_last: Option<(u8, i32, i32)>,
    last_paint_at: Instant,
    last_sent_pos: Option<Vector2>,
    last_sent_at: Instant,
    now_micros: i64,
    /// Mirrors `main.rs`'s
    /// `suppress_map_until_release` — latches at press-start whether a modal
    /// was open, held for the whole press (which spans several frames), so
    /// the click that closes an overlay can't also paint the cell behind it
    /// once it's gone.
    suppress_map_until_release: bool,
    /// Bug fix: mirrors `main.rs`'s stale-focus latch. Opening the header's
    /// "hexel" link (`window.open`, `_blank`) can switch the browser's
    /// active tab away from the canvas mid-gesture; the canvas's mouseup
    /// never lands if the release happens after that switch, so raylib's
    /// web platform (blur/focus-callback driven `IsWindowFocused`, canvas-
    /// scoped mouse state) reads stuck-"pressed" the moment the tab regains
    /// focus. `was_focused` tracks the previous frame's focus state so the
    /// unfocused->focused edge can be detected.
    was_focused: bool,
    /// True from that edge until every mouse button genuinely reads up
    /// again — see the same field's twin in `main.rs` for the full
    /// explanation. Held off level-triggered input (painting, panning) for
    /// as long as this is true.
    mouse_state_stale: bool,
    /// `None` if the audio device failed to init (no sound card,
    /// browser autoplay block, headless) — every call site degrades to
    /// silence instead of unwrapping. Backed by a leaked `'static`
    /// `RaylibAudio` (see `main`'s comment) — this `State` itself is never
    /// freed either (`Box::into_raw`, below), so leaking the audio device
    /// alongside it for the process's whole lifetime is the same tradeoff,
    /// not a new one.
    sfx: Option<sfx::Sfx<'static>>,
    /// Same leaked handle `sfx` was loaded from, kept here too so the header
    /// sound toggle can call `set_master_volume` without re-deriving it from
    /// `sfx` (which only exposes individual `Sound`/`Music` handles).
    audio: Option<&'static RaylibAudio>,
}

fn handle_message(state: &mut State, raw: &str) {
    let Ok(msg) = serde_json::from_str::<Value>(raw) else {
        return;
    };
    if let Some(id_token) = msg.get("IdentityToken") {
        if let Some(hex) = id_token.get("identity").and_then(identity_hex) {
            state.my_identity = Some(hex);
        }
    } else if let Some(initial) = msg.get("InitialSubscription") {
        // Reconnects replay the full table; drop stale local state first —
        // but only for the first snapshot of a connection. The painted map
        // arrives as a second, separate subscription (game.html's
        // STARTUP_TABLES then WORLD_TABLES), and clearing on that one would
        // wipe everything the first just delivered. `subscription_ready` is
        // reset to false whenever the socket isn't open, so it means exactly
        // "this connection has already had its first snapshot".
        if !state.subscription_ready {
            state.tables.clear();
        }
        if let Some(db_update) = initial.get("database_update") {
            state.tables.apply(db_update);
        }
        state.subscription_ready = true;
    } else if let Some(applied) = msg.get("SubscribeMultiApplied") {
        // What `SubscribeMulti` answers with instead of `InitialSubscription`
        // — same snapshot, under `update` rather than `database_update`. Both
        // are handled: the message a subscription replies with is decided by
        // which one game.html sent.
        if !state.subscription_ready {
            state.tables.clear();
        }
        if let Some(db_update) = applied.get("update") {
            state.tables.apply(db_update);
        }
        state.subscription_ready = true;
    } else if let Some(tx) = msg.get("TransactionUpdate") {
        // Received for our *own* reducer calls (we're the caller). A
        // rejected call (e.g. rate limit) carries `status: {"Failed": ..}`
        // instead, which simply has no `database_update` to apply.
        if let Some(db_update) = tx.get("status").and_then(|s| s.get("Committed")) {
            state.tables.apply(db_update);
        }
    } else if let Some(light) = msg.get("TransactionUpdateLight") {
        // What the server actually sends subscribers for *other* clients'
        // reducer calls — miss this and other players never move.
        if let Some(db_update) = light.get("update") {
            state.tables.apply(db_update);
        }
    }
}

/// Replace the compiled-in title bake with the freshly fetched one.
///
/// All three pieces move together or none do: the pose places the image in
/// the world and the stamp decides which cells are drawn live over it, so a
/// half-applied swap misplaces the map or double-draws the newest tiles. A
/// bad file simply leaves the embedded bake in place — it is decoration, and
/// A slightly stale backdrop beats a missing one.
fn swap_title_map(state: &mut State) {
    let Ok(png) = std::fs::read("/title-map-live.png") else {
        return;
    };
    let Ok(view_text) = std::fs::read_to_string("/title-map-live-view.txt") else {
        return;
    };
    let Some(view) = title_map::parse_view(&view_text) else {
        return;
    };
    let Some(texture) = title_map::load_png(&mut state.rl, &state.thread, &png) else {
        return;
    };
    state.title_map = Some(texture);
    state.title_map_view = view;
    state.title_map_baked_at = title_map::parse_baked_at(&view_text);
    // The intro eases from the pose the title was pinned at. If it was cached
    // before the swap it belongs to the old image, and pressing Draw would
    // jump.
    state.intro_from = None;
}

fn recenter(camera: &mut Camera2D, island: &IslandRow) {
    camera.target = island_world_center(island);
    camera.zoom = ISLAND_FIT_ZOOM;
    // Mouse-wheel/pinch zoom re-anchors `offset` to the cursor/fingers, so
    // reset it back to screen-center or the island lands off-target.
    camera.offset = Vector2::new(360.0, 360.0);
}

fn start_replay_with_duration(
    state: &mut State,
    island_filter: Option<u32>,
    duration_secs: f32,
) -> bool {
    // Reuse a replay already open on the same target rather than reopening
    // it. Reopening resets the event range to the whole file, which silently
    // discarded any `Cut` just made — so an export started from a trimmed
    // replay recorded the full history instead of what the slider showed.
    // Only the pace changes here; the (possibly narrowed) range is kept.
    let reusable = state.replay_clock.is_some()
        && state
            .recovered_replay
            .as_ref()
            .is_some_and(|recovered| recovered.island_filter == island_filter);
    if reusable {
        if let Some(recovered) = state.recovered_replay.as_mut() {
            if let Err(error) = recovered.reset() {
                state.ui_state.show_info_toast(error);
                return false;
            }
        }
        if let Some(clock) = state.replay_clock.as_mut() {
            clock.retime(duration_secs);
        }
        return true;
    }
    let recovered = match RecoveredReplay::open(island_filter) {
        Ok(recovered) => recovered,
        Err(error) => {
            state.ui_state.show_info_toast(error);
            return false;
        }
    };
    let total_events = recovered.playback_events();
    state.recovered_replay = Some(recovered);
    if let Some(island_id) = island_filter {
        state
            .ui_state
            .show_info_toast(format!("replaying island #{island_id}"));
    }
    let mut clock = replay::ReplayClock::new(
        // Recovered events are ordered by the time they happened. Replay pace
        // is deliberately controlled by the existing duration/speed UI, not
        // by the wall-clock gaps between them.
        [0, total_events as i64],
        duration_secs,
        state.now_micros,
    );
    if island_filter.is_none() {
        clock.enable_island_motion();
    }
    state.replay_clock = Some(clock);
    state.ui_state.title_active = false;
    state.centered_on_island = true;
    if island_filter.is_some() {
        // Individual replays draw the selected island in the canonical
        // origin slot (see `RecoveredReplay::display_slot`), never at its
        // mutable leaderboard rank.
        state.camera.target = world::axial_to_world(0, 0);
        state.camera.zoom = ISLAND_FIT_ZOOM;
    } else {
        let (target, zoom) = world_fit_with_min_zoom(
            &state.tables,
            state.camera.target,
            state.camera.zoom,
            replay::MIN_CAMERA_ZOOM,
        );
        state.camera.target = target;
        state.camera.zoom = zoom;
    }
    state.camera.offset = Vector2::new(360.0, 360.0);
    true
}

/// A replay request parked until the recovered history finishes downloading.
#[derive(Clone, Copy)]
struct PendingReplay {
    island_filter: Option<u32>,
    /// Export requests land back in `start_export_replay` so they still get
    /// the "Set speed and cut" hint; plain ones in `start_replay`.
    export: bool,
}

/// How far `hexelEnsureHistory` has got. Mirrors the compact strings
/// `window.hexelHistoryStatus` returns in game.html.
enum HistoryStatus {
    Idle,
    Loading,
    Ready,
    Error(String),
}

fn history_status() -> HistoryStatus {
    let raw = run_js("window.hexelHistoryStatus ? window.hexelHistoryStatus() : 'idle'");
    if raw == "ready" {
        return HistoryStatus::Ready;
    }
    if raw == "loading" {
        return HistoryStatus::Loading;
    }
    if let Some(message) = raw.strip_prefix("error:") {
        return HistoryStatus::Error(message.to_string());
    }
    HistoryStatus::Idle
}

/// Gate every replay entry point on the history actually being in MEMFS.
///
/// Returns true only when `RecoveredReplay::open` can succeed right now.
/// Since the download starts as soon as the title screen is up (see
/// `frame`), by the time anyone reaches Export it has almost always landed
/// and this is just a check. On the rare miss it parks the request and
/// leaves the caller to bail — `frame` replays it once the data arrives, so
/// the button press is honoured rather than dropped. No percentage: at
/// ~13 MiB on the wire the wait is short enough that a progress readout
/// draws more attention to it than it deserves.
fn ensure_history_ready(state: &mut State, island_filter: Option<u32>, export: bool) -> bool {
    if matches!(history_status(), HistoryStatus::Ready) {
        return true;
    }
    run_js("window.hexelEnsureHistory && window.hexelEnsureHistory()");
    state.history_pending = Some(PendingReplay {
        island_filter,
        export,
    });
    state
        .ui_state
        .show_info_toast("Preparing replay…".to_string());
    false
}

fn start_replay(state: &mut State, island_filter: Option<u32>) -> bool {
    if !ensure_history_ready(state, island_filter, false) {
        return false;
    }
    start_replay_with_duration(state, island_filter, replay::DEFAULT_DURATION_SECS)
}

/// Approximate byte count as `~N KB` / `~N.N MB`. These are projections from
/// the capture settings, not measurements — nothing has been recorded when
/// they are shown — hence the tilde.
fn format_bytes(bytes: f32) -> String {
    if bytes >= 1024.0 * 1024.0 {
        format!("~{:.1} MB", bytes / (1024.0 * 1024.0))
    } else {
        format!("~{:.0} KB", (bytes / 1024.0).max(1.0))
    }
}

/// Open the replay for an export without recording anything yet.
///
/// `island_filter`: `Some(id)` exports just that island, `None` the whole
/// world — same convention as `start_replay`/`RecoveredReplay`.
///
/// Recording used to start immediately at a fixed duration, which meant the
/// file never reflected a `Cut` or a speed the player picked afterwards —
/// there was no point at which those choices could still be made. Opening the
/// replay instead leaves it interactive, and the overlay's Download button
/// captures whatever state it is in by then. Every open replay offers
/// download, so this only differs from `start_replay` by the hint.
fn start_export_replay(state: &mut State, island_filter: Option<u32>) {
    if !ensure_history_ready(state, island_filter, true) {
        return;
    }
    if !start_replay_with_duration(state, island_filter, replay::DEFAULT_DURATION_SECS) {
        return;
    }
    state.export_saved_for = 0.0;
    state
        .ui_state
        .show_info_toast("Set speed and cut, then press Download".to_string());
}

/// Record the replay exactly as it currently stands — same range (after any
/// cut) and same speed — then hand the file to the browser.
fn begin_export_recording(state: &mut State, format: replay::AnimationFormat) {
    let island_filter = state
        .recovered_replay
        .as_ref()
        .and_then(|recovered| recovered.island_filter);
    // Rewind so the capture covers the full window rather than starting from
    // wherever the playhead was parked.
    if let Some(recovered) = state.recovered_replay.as_mut() {
        if let Err(error) = recovered.reset() {
            state.ui_state.show_info_toast(error);
            return;
        }
    }
    if let Some(clock) = state.replay_clock.as_mut() {
        clock.restart();
    }
    match format {
        replay::AnimationFormat::Gif => start_gif_export(state, island_filter),
        replay::AnimationFormat::Webm => start_video_export(state, island_filter),
    }
}

fn start_gif_export(state: &mut State, island_filter: Option<u32>) {
    if unsafe { hexel_gif_begin(GIF_EXPORT_SIZE, GIF_EXPORT_SIZE) } != 0 {
        state.gif_recording = true;
        state.gif_frame_counter = 0;
        state.clean_timeline_export = true;
        // `finish_gif_recording` reads this rather than re-deriving the
        // scope from `state.recovered_replay`, which a mid-recording close
        // (see `close_replay` in `frame`) may already have cleared by then.
        state.gif_export_island = island_filter;
    } else {
        state
            .ui_state
            .show_info_toast("could not start GIF export".to_string());
    }
}

fn start_video_export(state: &mut State, island_filter: Option<u32>) {
    state.replay_recording = true;
    state.clean_timeline_export = true;
    let scope = if island_filter.is_some() {
        "island"
    } else {
        "world"
    };
    if run_js(&format!(
        "Boolean(window.stdb && window.stdb.startReplayRecording('{scope}'))"
    )) != "true"
    {
        state.replay_recording = false;
        state.clean_timeline_export = false;
        state.replay_clock = None;
        state.recovered_replay = None;
        state
            .ui_state
            .show_info_toast("video export is not supported by this browser".to_string());
    }
}

extern "C" fn on_frame(arg: *mut c_void) {
    let state = unsafe { &mut *(arg as *mut State) };
    frame(state);
}

/// Sound rework: mirrors `main.rs`'s `GIFT_CLAIM_WINDOW` — how long after a
/// `claim_gift` reducer call the resulting XP bump is attributed to the
/// gift reward roll instead of a plain xp.mp3 blip.
const GIFT_CLAIM_WINDOW: Duration = Duration::from_secs(5);

fn frame(state: &mut State) {
    // Bug fix (see `mouse_state_stale`'s doc comment): latch stale on the
    // unfocused->focused edge, clear it once every button genuinely reads
    // up again.
    let focused_now = state.rl.is_window_focused();
    if focused_now && !state.was_focused {
        state.mouse_state_stale = true;
    }
    state.was_focused = focused_now;
    if state.mouse_state_stale
        && !state
            .rl
            .is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT)
        && !state
            .rl
            .is_mouse_button_down(MouseButton::MOUSE_BUTTON_MIDDLE)
        && !state
            .rl
            .is_mouse_button_down(MouseButton::MOUSE_BUTTON_RIGHT)
    {
        state.mouse_state_stale = false;
    }
    // One JS call pulls in everything the socket received since last frame.
    let raw = run_js("window.stdb ? window.stdb.frame() : '{}'");
    let data: FrameData = serde_json::from_str(&raw).unwrap_or_default();
    let connection_open = data.status == "open";
    if !connection_open {
        // Do not let stale rows from a previous socket make Draw reappear
        // during a reconnect; the replacement snapshot must land first.
        state.subscription_ready = false;
    }
    state.now_micros = data.now_micros;
    if state.my_identity.is_none() {
        state.my_identity = data.identity.as_deref().map(normalize_identity);
    }
    for msg in &data.msgs {
        handle_message(state, msg);
    }
    let startup_ready = connection_open
        && state.subscription_ready
        && state.my_identity.as_deref().is_some_and(|me| {
            state.tables.users.contains_key(me)
                && my_island(&state.tables, me).is_some()
                && state
                    .tables
                    .inventory
                    .values()
                    .any(|row| row.owner_hex == me)
        });
    state.ui_state.set_startup_progress(
        connection_open,
        data.status == "closed",
        startup_ready,
        state.tables.islands.len(),
    );
    // A rejected Import — the current account
    // and connection are untouched (see `importToken` in game.html), so this
    // is just user feedback, not a reconnect.
    if let Some(err) = data.import_error {
        state.ui_state.show_info_toast(err);
    }
    // Kicked off from the first frame, not from `main`: emscripten's
    // filesystem has to be up before JS can write the fetched files into it.
    if !state.title_map_requested {
        state.title_map_requested = true;
        run_js("window.hexelLoadTitleMap && window.hexelLoadTitleMap()");
        // Prefetch the replay history in the same breath. It is ~13 MiB on
        // the wire, so it lands quietly while the player is still on the
        // title screen, and Export then opens instantly instead of stopping
        // to download. Kicked off here rather than from `main` for the same
        // reason as the title map: emscripten's filesystem has to exist
        // before JS can write the result into it.
        run_js("window.hexelEnsureHistory && window.hexelEnsureHistory()");
    }
    if data.title_map_ready {
        swap_title_map(state);
    }

    // Drive a replay that was requested before the history had downloaded.
    // The toast is re-shown every frame rather than posted once: each call
    // resets `shown_at`, so it neither expires mid-download nor needs a
    // second, persistent widget just for this.
    if let Some(pending) = state.history_pending {
        match history_status() {
            HistoryStatus::Ready => {
                state.history_pending = None;
                if pending.export {
                    start_export_replay(state, pending.island_filter);
                } else {
                    start_replay(state, pending.island_filter);
                }
            }
            HistoryStatus::Loading => {
                state.ui_state.show_info_toast("Preparing replay…".to_string());
            }
            HistoryStatus::Error(message) => {
                state.history_pending = None;
                state
                    .ui_state
                    .show_info_toast(format!("Replay history unavailable: {message}"));
            }
            // The fetch hasn't reported in yet (or a reload cleared it):
            // ask again rather than waiting forever on a dropped request.
            HistoryStatus::Idle => {
                run_js("window.hexelEnsureHistory && window.hexelEnsureHistory()");
            }
        }
    }

    let mut stop_replay_recording = false;
    let mut finish_gif_recording = false;
    let close_replay = state
        .replay_clock
        .as_mut()
        .is_some_and(|clock| replay::handle_input(&mut state.rl, clock));
    if close_replay {
        if state.replay_recording {
            state.replay_recording = false;
            stop_replay_recording = true;
        }
        if state.gif_recording {
            finish_gif_recording = true;
        }
        state.replay_clock = None;
        state.recovered_replay = None;
    } else if let Some(clock) = state.replay_clock.as_mut() {
        clock.tick(state.rl.get_frame_time());
    }
    // Download button lifecycle. It stays in place through every state so the
    // export always reports what it is doing, rather than vanishing on click.
    // "Downloaded" is a timed confirmation, not a terminal state. Coming back
    // to the tab ends it outright: the badge has already served its purpose by
    // the time the player has been away and returned.
    state.export_saved_for = if data.page_returned {
        0.0
    } else {
        (state.export_saved_for - state.rl.get_frame_time()).max(0.0)
    };
    // Every open replay can be downloaded, whichever door it was opened by:
    // the world replay reached from the menu used to be the one place the
    // button never appeared, because only the export modal armed it.
    let download_state = if state.gif_recording || state.replay_recording {
        replay::DownloadState::Working
    } else if state.export_saved_for > 0.0 {
        replay::DownloadState::Done
    } else {
        replay::DownloadState::Ready
    };
    let mut chosen_format = None;
    if let Some(clock) = state.replay_clock.as_mut() {
        clock.set_download_state(download_state);
        // Refreshed while the prompt is open so the figures track the speed;
        // both scale linearly with how long the capture actually runs.
        if clock.prompt_open() {
            let seconds = clock.real_duration_secs();
            let gif_frame_bytes =
                (GIF_EXPORT_SIZE * GIF_EXPORT_SIZE) as f32 * GIF_BYTES_PER_PIXEL;
            clock.set_format_estimates(
                format_bytes(seconds * WEBM_BITS_PER_SEC / 8.0),
                format_bytes(seconds * GIF_CAPTURE_FPS * gif_frame_bytes),
            );
        }
        let _ = clock.take_download_request();
        chosen_format = clock.take_format_choice();
    }
    if let Some(format) = chosen_format {
        begin_export_recording(state, format);
    }
    // A `Cut` narrows the clock's timestamp range, but the recovered replay
    // is paced over event indices, so it has to be narrowed by the same
    // fractions or the cut would change neither its playback nor its count.
    if let Some((start_fraction, end_fraction)) = state
        .replay_clock
        .as_mut()
        .and_then(replay::ReplayClock::take_applied_cut)
    {
        if let Some(recovered) = state.recovered_replay.as_mut() {
            if let Err(error) = recovered.narrow(start_fraction, end_fraction) {
                state.replay_clock = None;
                state.recovered_replay = None;
                state.ui_state.show_info_toast(error);
            }
        }
    }
    if let (Some(clock), Some(recovered)) =
        (state.replay_clock.as_ref(), state.recovered_replay.as_mut())
    {
        if let Err(error) = recovered.advance_to(clock.progress()) {
            state.replay_clock = None;
            state.recovered_replay = None;
            state.ui_state.show_info_toast(error);
        }
    }
    if state
        .replay_clock
        .as_ref()
        .is_some_and(replay::ReplayClock::is_finished)
        && state.replay_recording
    {
        state.replay_recording = false;
        stop_replay_recording = true;
    }
    if state
        .replay_clock
        .as_ref()
        .is_some_and(replay::ReplayClock::is_finished)
        && state.gif_recording
    {
        finish_gif_recording = true;
    }
    let mut replay_mode = state.replay_clock.is_some();
    let replay_closed_this_frame = close_replay;

    let me = state.my_identity.clone();
    let me = me.as_deref();
    // Theme music: mirrors `main.rs` exactly. Gating on `!title_active`
    // (title dismissed via a real click) also satisfies the web autoplay
    // policy — the browser's AudioContext is created suspended and only
    // resumes on a user gesture, so by the time this fires it's already
    // resumed and the stream is heard from 0:00 (see `main`'s comment on
    // `RaylibAudio::init_audio_device` above).
    if let Some(s) = &state.sfx {
        if !state.music_started && !state.ui_state.title_active {
            s.theme.play_stream();
            state.music_started = true;
        }
        if state.music_started {
            s.theme.update_stream();
        }
    }
    let hexa_zoom_locked =
        !replay_mode && me.is_some_and(|id| state.tables.hexa_clusters.contains_key(id));
    if hexa_zoom_locked {
        let rate = (1.0
            - (-state.rl.get_frame_time() / world::constants::HEXA_ZOOM_LERP_SECS).exp())
        .clamp(0.0, 1.0);
        state.camera.zoom += (ISLAND_FIT_ZOOM - state.camera.zoom) * rate;
        if (state.camera.zoom - ISLAND_FIT_ZOOM).abs() < 0.001 {
            state.camera.zoom = ISLAND_FIT_ZOOM;
        }
    }
    let mouse_screen = state.rl.get_mouse_position();
    let mouse_world = state.rl.get_screen_to_world2D(mouse_screen, state.camera);

    // Mirrors `main.rs` exactly — current drifted world
    // position of the single active gift (if any) and whether the mouse is
    // within claim range of it right now.
    let active_gift: Option<(u64, Vector2, f32)> = (!replay_mode)
        .then(|| state.tables.gifts.iter().next())
        .flatten()
        .map(|(&id, g)| {
            let elapsed =
                ((state.now_micros - g.spawned_at_micros).max(0) as f64 / 1_000_000.0) as f32;
            let pos = world::gift_drift_pos(Vector2::new(g.x, g.y), elapsed);
            (id, pos, elapsed)
        });
    let gift_hit = active_gift.is_some_and(|(_, pos, _)| {
        let dx = mouse_world.x - pos.x;
        let dy = mouse_world.y - pos.y;
        (dx * dx + dy * dy).sqrt() <= GIFT_CLAIM_DIST
    });
    // Mirrors `main.rs` exactly: the header/footer HUD bands sit ON TOP of
    // the map, but the screen coordinate underneath still maps to SOME world
    // tile via the camera transform. Gates every mouse-driven world-mutation
    // block below so clicking a HUD button never also paints/erases/eyedrops/
    // long-press-merges whatever tile happens to lie beneath it.
    // `!title_active`: mirrors `main.rs` — the title screen covers the whole
    // screen, so nothing is "over the map" until Draw dismisses it.
    let over_map_area = !state.ui_state.title_active
        && mouse_screen.y > ui::HEADER_H
        && mouse_screen.y < (720.0 - ui::FOOTER_H);

    // Launch intro — mirrors `main.rs` exactly. Camera starts
    // framing the whole occupied world and eases to the player's island over
    // `INTRO_DURATION`; any input skips straight to the final pose.
    //
    // Title screen (author-requested): mirrors `main.rs` — while it's up,
    // hold the camera on the whole-occupied-world pose so the
    // semi-transparent backdrop shows a hint of the full map; the intro
    // only starts once the Draw button dismisses the title, easing from
    // this exact pose (`intro_from` calls the same `world_fit`).
    if state.ui_state.title_active {
        // Mirrors main.rs: pinned to the pose `title-map.png` was baked at,
        // since that image is what's behind the title now. The live cursors
        // drawn on top go through this same camera, so it has to match.
        let (target, zoom) = state.title_map_view;
        state.camera.target = target;
        state.camera.zoom = zoom;
        state.camera.offset = Vector2::new(360.0, 360.0);
    } else if let Some(me) = me {
        if !state.centered_on_island {
            if let Some((island, _)) = my_island(&state.tables, me) {
                let to_target = island_world_center(island);
                let to_zoom = ISLAND_FIT_ZOOM;
                let start = *state.intro_started_at.get_or_insert_with(Instant::now);
                // Mirrors main.rs: ease from the pose the title (and the
                // baked map) was pinned at, so pressing Draw doesn't jump.
                let (from_target, from_zoom) =
                    *state.intro_from.get_or_insert(state.title_map_view);
                let any_input = state
                    .rl
                    .is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT)
                    || state
                        .rl
                        .is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_MIDDLE)
                    || state
                        .rl
                        .is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_RIGHT)
                    || state.rl.get_mouse_wheel_move() != 0.0
                    || state.rl.get_touch_point_count() > 0
                    || state.rl.get_key_pressed().is_some();
                let t =
                    (start.elapsed().as_secs_f32() / INTRO_DURATION.as_secs_f32()).clamp(0.0, 1.0);
                if any_input || t >= 1.0 {
                    state.camera.target = to_target;
                    state.camera.zoom = to_zoom;
                    state.centered_on_island = true;
                } else {
                    // Ease-in-out-cubic: slow start, fast middle, gentle
                    // landing (author-requested, other_ideas.md; mirrors
                    // `main.rs`).
                    let ease = world::ease_in_out_cubic(t);
                    state.camera.target = Vector2::new(
                        from_target.x + (to_target.x - from_target.x) * ease,
                        from_target.y + (to_target.y - from_target.y) * ease,
                    );
                    state.camera.zoom = from_zoom + (to_zoom - from_zoom) * ease;
                }
                state.camera.offset = Vector2::new(360.0, 360.0);
            }
        }
    }

    // Sound rework: mirrors `main.rs`'s `reward_sound_this_frame` — a
    // new_color/hexa-grant sound already fired this frame, so the xp/level
    // block below shouldn't also blip xp.mp3 for the same XP bump.
    let mut reward_sound_this_frame = false;
    // Inventory-insert watch (merge toast): toast + last3 nudge when a new
    // row for `me` appears. Mirrors `main.rs` exactly.
    if let Some(me) = me {
        if !state.merge_events_seeded {
            state.known_merge_event_ids = state.tables.merge_events.keys().copied().collect();
            state.merge_events_seeded = true;
        } else {
            for (&id, event) in &state.tables.merge_events {
                if state.known_merge_event_ids.insert(id)
                    && (event.a_hex == me || event.b_hex == me)
                {
                    state.ui_state.note_used_color(ui::RecentColor {
                        hue: event.merged_hue,
                        sat: event.merged_sat,
                        val: event.merged_val,
                    });
                    if let Some(s) = &state.sfx {
                        s.merge.play();
                    }
                }
            }
        }
        if !state.hexa_events_seeded {
            state.known_hexa_event_ids = state.tables.hexa_events.keys().copied().collect();
            state.hexa_events_seeded = true;
        } else {
            for (&event_id, _) in &state.tables.hexa_events {
                if state.known_hexa_event_ids.insert(event_id)
                    && state
                        .tables
                        .hexa_clusters
                        .get(me)
                        .is_some_and(|row| row.ignited)
                {
                    state.ui_state.show_hexa_success_popup();
                    if let Some(s) = &state.sfx {
                        s.new_color.play();
                    }
                    reward_sound_this_frame = true;
                }
            }
        }
        if !state.inventory_seeded {
            if state.tables.inventory.values().any(|i| i.owner_hex == me) {
                state.known_inventory_ids = state.tables.inventory.keys().copied().collect();
                state.inventory_seeded = true;
            }
        } else {
            for (&id, inv) in &state.tables.inventory {
                if state.known_inventory_ids.insert(id) && inv.owner_hex == me {
                    // Mirrors `main.rs` — a NEW obtained_with-
                    // less row after the initial seed can only be
                    // `reset_account`'s reseed, never a merge.
                    if inv.from_gift {
                        state.ui_state.show_gift_toast(inv.hue);
                        state.pending_gift_claim = None;
                        reward_sound_this_frame = true;
                        if let Some(s) = &state.sfx {
                            s.play_gift_reward(
                                &s.new_color,
                                state.rl.get_random_value(0..=2),
                                state.rl.get_random_value(0..=99),
                            );
                        }
                    } else if inv.obtained_with_hex.is_none() {
                        // Mirrors `main.rs` — a Hexa-pooled grant also
                        // leaves `obtained_with_hex: None`; told apart from a
                        // `reset_account` reseed by joining against a
                        // `hexa_event` row stamped with the identical
                        // `ctx.timestamp` (see `apply_hexa`'s server-side
                        // comment).
                        if state
                            .tables
                            .hexa_events
                            .values()
                            .any(|e| e.at_micros == inv.obtained_at_micros)
                        {
                            state.ui_state.show_hexa_toast(inv.hue);
                            if let Some(s) = &state.sfx {
                                s.new_color.play();
                            }
                            reward_sound_this_frame = true;
                        } else {
                            state.ui_state.note_reset_hue(inv.hue);
                        }
                    } else {
                        let label = inv.obtained_with_hex.as_deref().map_or_else(
                            || "someone".to_string(),
                            |p| player_label(&state.tables, p),
                        );
                        // Mirrors `main.rs`: use the same
                        // `obtained_at`/`MergeEvent.at` timestamp join
                        // the Hexa event branch above already uses.
                        let merge_from = state
                            .tables
                            .merge_events
                            .values()
                            .find(|e| {
                                e.at_micros == inv.obtained_at_micros
                                    && (e.a_hex == me || e.b_hex == me)
                            })
                            .map(|e| {
                                if e.a_hex == me {
                                    (e.hue_a, e.hue_b)
                                } else {
                                    (e.hue_b, e.hue_a)
                                }
                            });
                        let color = state
                            .tables
                            .merge_events
                            .values()
                            .find(|e| {
                                e.at_micros == inv.obtained_at_micros
                                    && (e.a_hex == me || e.b_hex == me)
                            })
                            .map(|e| ui::RecentColor {
                                hue: e.merged_hue,
                                sat: e.merged_sat,
                                val: e.merged_val,
                            })
                            .or_else(|| {
                                state.tables.users.get(me).map(|u| ui::RecentColor {
                                    hue: inv.hue,
                                    sat: u.sat,
                                    val: u.val,
                                })
                            })
                            .unwrap_or(ui::RecentColor {
                                hue: inv.hue,
                                sat: world::sat_cap(0),
                                val: 90,
                            });
                        state.ui_state.show_merge_toast(color, &label, merge_from);
                        if merge_from.is_none() {
                            if let Some(s) = &state.sfx {
                                s.merge.play();
                            }
                        }
                    }
                }
            }
        }
        // Mirrors `main.rs` — seed the last-3 ring with the
        // caller's current hue the first frame it's known.
        if let Some(user) = state.tables.users.get(me) {
            state.ui_state.seed_recent_once(ui::RecentColor {
                hue: user.hue,
                sat: user.sat,
                val: user.val,
            });
        }
    }

    let online = state.tables.users.values().filter(|u| u.online).count();
    let total = state.tables.users.len();

    // Mirrors `main.rs` — latch at
    // press-start whether a modal was open, held for the whole press (a
    // click's press and release land on different frames), so the click
    // that closes an overlay can't also paint the cell behind it on a later
    // frame where the button is still held but the overlay's already gone.
    if state
        .rl
        .is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT)
    {
        // Mirrors `main.rs` — `any_modal_open`
        // deliberately excludes the foreign-island hover tooltip, which has
        // no interactive chrome and must not block map input.
        state.suppress_map_until_release = state.ui_state.any_modal_open();
        // Mirrors `main.rs` — a press landing on the gift claims it
        // immediately and consumes the whole gesture so the same press can't
        // also start a paint stroke or long-press underneath it.
        if !state.suppress_map_until_release && over_map_area && gift_hit {
            if let Some((gift_id, _, _)) = active_gift {
                call_reducer("claim_gift", serde_json::json!([gift_id]));
                state.pending_gift_claim = Some(Instant::now());
            }
            state.suppress_map_until_release = true;
        }
    }
    // Cleared whenever the button is simply not down, rather than only on the
    // release edge. A release that never reaches the canvas — the pointer
    // leaves it, or the browser takes focus for a download — used to leave
    // this latched, and with it latched the whole map is inert: no painting,
    // no panning, and no `set_pos`, so everyone else sees your cursor frozen
    // where you left it.
    if !state
        .rl
        .is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT)
    {
        state.suppress_map_until_release = false;
    }

    // HUD: snapshot server state, run widget input, apply resulting reducer
    // calls. Must run before the map-input blocks below so they can see
    // `ui_state.any_modal_open()`.
    // `Some(name)` marks this frame as the clean, camera-centred export
    // card. It is intentionally ephemeral: after the canvas is captured at
    // the end of this frame, the normal game returns immediately.
    let mut export_subject: Option<ui::ExportSubject> = None;
    if let Some(me) = me {
        let user = state.tables.users.get(me);
        let hues: Vec<u16> = state
            .tables
            .inventory
            .values()
            .filter(|i| i.owner_hex == me)
            .map(|i| i.hue)
            .collect();
        let (hue, sat, val) = user.map_or((0, 40, 100), |u| (u.hue, u.sat, u.val));
        let xp = user.map_or(0, |u| u.xp);
        let locked = user.is_some_and(|u| u.locked);
        let level = world::level_of(xp);
        // Level-up feedback: mirrors `main.rs` exactly.
        let leveled = state.last_level.is_some_and(|prev| level > prev);
        if state.last_level.is_some_and(|prev| {
            prev < shared::constants::HEXA_UNLOCK_LEVEL
                && level >= shared::constants::HEXA_UNLOCK_LEVEL
        }) {
            state.ui_state.show_hexa_unlocked_toast();
            if let Some(s) = &state.sfx {
                s.levelup.play();
            }
        } else if leveled {
            state
                .ui_state
                .show_levelup_toast(level, world::sat_cap(level), hue);
            if let Some(s) = &state.sfx {
                s.levelup.play();
            }
        }
        state.last_level = Some(level);
        // Sound rework: mirrors `main.rs`'s xp-delta gating exactly — see
        // that file's comment for the reasoning.
        let xp_delta = state.last_xp.map_or(0, |prev| xp.saturating_sub(prev));
        if xp_delta >= 2 {
            if !leveled && !reward_sound_this_frame {
                if let Some(s) = &state.sfx {
                    if state
                        .pending_gift_claim
                        .is_some_and(|t| t.elapsed() <= GIFT_CLAIM_WINDOW)
                    {
                        s.play_gift_reward(
                            &s.xp,
                            state.rl.get_random_value(0..=2),
                            state.rl.get_random_value(0..=99),
                        );
                    } else {
                        s.xp.play();
                    }
                }
            }
            state.pending_gift_claim = None;
        }
        state.last_xp = Some(xp);
        state
            .ui_state
            .sync_name_once(user.and_then(|u| u.name.as_ref()));
        // Mirrors `main.rs`'s live-refresh so the Like button
        // reflects the reducer's result immediately, not only after a page
        // reload (the popup used to be a one-time snapshot from open time).
        if let Some(island_id) = state.ui_state.island_popup.as_ref().map(|p| p.island_id) {
            if let Some(island) = state.tables.islands.get(&island_id) {
                let likes = island.likes;
                let liked = already_liked(&state.tables, island_id, me);
                let border_color = resolve_border_color(&state.tables, island);
                state.ui_state.refresh_island_popup(
                    likes,
                    liked,
                    island.border_hidden,
                    border_color,
                );
            }
        }

        let short_id = me.to_string();
        // Re-rank countdown: only `Some` while the target is still ahead
        // of the browser clock reported this frame.
        let rerank_secs = state
            .tables
            .configs
            .get(&0)
            .and_then(|c| c.next_rerank_at_micros)
            .map(|t| t - state.now_micros)
            .filter(|&d| d > 0)
            .map(|d| (d as f64 / 1_000_000.0).ceil() as i64);
        let info = ui::HudInfo {
            short_id: short_hex(&short_id),
            level,
            xp,
            camera_target: state.camera.target,
            online,
            total,
            locked,
            brush: (hue, sat, val),
            sat_cap: world::sat_cap(level),
            hues: &hues,
            show_token_import: true,
            rerank_secs,
            link_id: my_island(&state.tables, me).and_then(|(isl, _)| isl.itch_rate_id),
            is_admin: is_admin(&state.tables, me),
        };
        let actions = if replay_mode || replay_closed_this_frame {
            ui::Actions::default()
        } else {
            ui::handle_input(&mut state.rl, &mut state.ui_state, &info)
        };
        // Header sound toggle: master volume covers sfx and the theme music
        // in one call, so no per-call-site gating is needed (matches main.rs).
        if let Some(audio) = state.audio {
            audio.set_master_volume(if state.ui_state.sound_on { 1.0 } else { 0.0 });
        }
        if let Some((h, s, v)) = actions.set_brush {
            call_reducer("set_brush", serde_json::json!([h, s, v]));
        }
        if let Some(name) = actions.set_name {
            call_reducer("set_name", serde_json::json!([name]));
        }
        if let Some(locked) = actions.set_lock {
            call_reducer("set_lock", serde_json::json!([locked]));
        }
        if actions.copy_token {
            run_js("window.stdb && window.stdb.copyToken()");
        }
        if let Some(token) = actions.import_token {
            let js_token = serde_json::to_string(&token).unwrap();
            run_js(&format!(
                "window.stdb && window.stdb.importToken({js_token})"
            ));
        }
        if actions.reset_account {
            call_reducer("reset_account", serde_json::json!([]));
        }
        if actions.delete_account {
            call_reducer("delete_account", serde_json::json!([]));
        }
        if let Some(island_id) = actions.like_island {
            call_reducer("like_island", serde_json::json!([island_id]));
        }
        if let Some(island_id) = actions.unlike_island {
            call_reducer("unlike_island", serde_json::json!([island_id]));
        }
        if let Some(rate_id) = actions.set_island_link {
            call_reducer("set_island_link", serde_json::json!([rate_id]));
        }
        if actions.set_island_border {
            call_reducer("set_island_border", serde_json::json!([]));
        }
        if actions.disable_island_border {
            call_reducer("disable_island_border", serde_json::json!([]));
        }
        if actions.show_island_border {
            call_reducer("show_island_border", serde_json::json!([]));
        }
        if let Some((island_id, name, likes, xp)) = actions.admin_save_island {
            call_reducer(
                "admin_update_island",
                serde_json::json!([island_id, name, likes, xp]),
            );
        }
        if let Some(island_id) = actions.admin_delete_island {
            call_reducer("admin_delete_island", serde_json::json!([island_id]));
        }
        if actions.admin_force_rerank {
            call_reducer("admin_force_rerank", serde_json::json!([]));
        }
        if let Some((island_id, rate_id)) = actions.click_link {
            // Plan.md web opens the rate page via `window.open`, unlike
            // native's `OpenURL` — JSON-escaped the same way `call_reducer`
            // escapes its args, though `rate_id` is server-validated numeric
            // so this is defense in depth rather than a real injection risk.
            let url = format!("https://itch.io/jam/raylib-6x-gamejam/rate/{rate_id}");
            let js_url = serde_json::to_string(&url).unwrap();
            run_js(&format!("window.open({js_url}, '_blank')"));
            call_reducer("click_link", serde_json::json!([island_id]));
        }
        if let Some(rate_id) = actions.open_own_link {
            let url = format!("https://itch.io/jam/raylib-6x-gamejam/rate/{rate_id}");
            let js_url = serde_json::to_string(&url).unwrap();
            run_js(&format!("window.open({js_url}, '_blank')"));
        }
        if actions.open_project_page {
            run_js("window.open('https://itch.io/jam/raylib-6x-gamejam/rate/4767021', '_blank')");
        }
        // Footer button replacing the old "click your own
        // island" gesture, which just painted instead of opening the popup.
        // Discarding the reference half as `_` (rather than binding it) ends
        // its borrow of `state.tables` right here, so the `&mut state` call
        // below is free to proceed.
        if actions.open_own_island {
            if let Some((_, island_id)) = my_island(&state.tables, me) {
                open_island_info(state, island_id);
            }
        }
        if actions.center_camera {
            if let Some((island, _)) = my_island(&state.tables, me) {
                recenter(&mut state.camera, island);
            }
        }
        if actions.center_world {
            let (target, zoom) = world_fit(&state.tables, state.camera.target, state.camera.zoom);
            state.camera.target = target;
            state.camera.zoom = zoom;
            state.camera.offset = Vector2::new(360.0, 360.0);
        }
        if actions.arm_island_export {
            state.ui_state.tool = ui::Tool::IslandExport;
            state.ui_state.show_info_toast(
                "Export: click an island, or empty map for the whole world".to_string(),
            );
        }
        match actions.export_image {
            Some(ui::ExportTarget::Island(island_id)) => {
                if let Some(island) = state.tables.islands.get(&island_id) {
                    recenter(&mut state.camera, island);
                    export_subject = Some(ui::ExportSubject::Island {
                        owner: player_label(&state.tables, &island.owner_hex),
                        link_id: island.itch_rate_id,
                    });
                }
            }
            Some(ui::ExportTarget::World) => {
                let (target, zoom) =
                    world_fit(&state.tables, state.camera.target, state.camera.zoom);
                state.camera.target = target;
                state.camera.zoom = zoom;
                state.camera.offset = Vector2::new(360.0, 360.0);
                export_subject = Some(ui::ExportSubject::World);
            }
            None => {}
        }
        if let Some(target) = actions.export_island_animation {
            start_export_replay(state, target.island_id());
            replay_mode = true;
        }
        if actions.start_replay {
            start_replay(state, None);
            replay_mode = true;
        }
    }
    // Mirrors `main.rs`.
    let map_input_allowed = !replay_mode
        && !replay_closed_this_frame
        && !state.suppress_map_until_release
        && !state.ui_state.any_modal_open();

    // Two-finger pinch/pan (touch); single-finger tap/drag is already
    // translated to ordinary mouse events by raylib's web backend, so the
    // mouse-driven paint/pan/long-press code below covers it unchanged.
    let touch_count = state.rl.get_touch_point_count();
    let gesturing = touch_count >= 2;
    if map_input_allowed && gesturing {
        let t0 = state.rl.get_touch_position(0);
        let t1 = state.rl.get_touch_position(1);
        let mid = Vector2::new((t0.x + t1.x) / 2.0, (t0.y + t1.y) / 2.0);
        let dist = ((t1.x - t0.x).powi(2) + (t1.y - t0.y).powi(2))
            .sqrt()
            .max(1.0);
        if state.pinch.is_none() {
            let anchor_world = state.rl.get_screen_to_world2D(mid, state.camera);
            state.pinch = Some(Pinch {
                anchor_world,
                start_dist: dist,
                start_zoom: state.camera.zoom,
            });
        }
        let pinch = state.pinch.as_ref().unwrap();
        if !hexa_zoom_locked {
            state.camera.zoom = (pinch.start_zoom * (dist / pinch.start_dist)).clamp(0.25, 60.0);
        }
        state.camera.offset = mid;
        state.camera.target = pinch.anchor_world;
    } else {
        state.pinch = None;
    }

    // Zoom toward the cursor (official raylib recipe), desktop-browser mice
    // only — touch pinch is handled above.
    let wheel = if map_input_allowed && !gesturing && !hexa_zoom_locked {
        state.rl.get_mouse_wheel_move()
    } else {
        0.0
    };
    if wheel != 0.0 {
        state.camera.offset = mouse_screen;
        state.camera.target = mouse_world;
        state.camera.zoom = (state.camera.zoom * (1.0 + wheel * 0.1)).clamp(0.25, 60.0);
    }

    // WASD/arrow-key pan, Q/E zoom — mirrors `main.rs` exactly,
    // including the text-field-focus guard.
    if map_input_allowed && !gesturing && !state.ui_state.text_field_focused() {
        let dt = state.rl.get_frame_time();
        let mut dx = 0.0;
        let mut dy = 0.0;
        if state.rl.is_key_down(KeyboardKey::KEY_LEFT) || state.rl.is_key_down(KeyboardKey::KEY_A) {
            dx -= 1.0;
        }
        if state.rl.is_key_down(KeyboardKey::KEY_RIGHT) || state.rl.is_key_down(KeyboardKey::KEY_D)
        {
            dx += 1.0;
        }
        if state.rl.is_key_down(KeyboardKey::KEY_UP) || state.rl.is_key_down(KeyboardKey::KEY_W) {
            dy -= 1.0;
        }
        if state.rl.is_key_down(KeyboardKey::KEY_DOWN) || state.rl.is_key_down(KeyboardKey::KEY_S) {
            dy += 1.0;
        }
        if dx != 0.0 || dy != 0.0 {
            let speed = KEY_PAN_SPEED / state.camera.zoom;
            state.camera.target.x += dx * speed * dt;
            state.camera.target.y += dy * speed * dt;
        }
        if !hexa_zoom_locked && state.rl.is_key_down(KeyboardKey::KEY_Q) {
            state.camera.zoom = (state.camera.zoom * (1.0 - KEY_ZOOM_RATE * dt)).clamp(0.25, 60.0);
        }
        if !hexa_zoom_locked && state.rl.is_key_down(KeyboardKey::KEY_E) {
            state.camera.zoom = (state.camera.zoom * (1.0 + KEY_ZOOM_RATE * dt)).clamp(0.25, 60.0);
        }
    }

    // The Move tool makes plain left-drag pan too, no Shift
    // needed — mirrors `main.rs`.
    let panning = map_input_allowed
        && !gesturing
        && !state.mouse_state_stale
        && (state
            .rl
            .is_mouse_button_down(MouseButton::MOUSE_BUTTON_MIDDLE)
            || state
                .rl
                .is_mouse_button_down(MouseButton::MOUSE_BUTTON_RIGHT)
            || (state.rl.is_key_down(KeyboardKey::KEY_LEFT_SHIFT)
                && state
                    .rl
                    .is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT))
            || (state.ui_state.tool == ui::Tool::Move
                && state
                    .rl
                    .is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT)));
    if panning {
        let delta = state.rl.get_mouse_delta();
        state.camera.target.x -= delta.x / state.camera.zoom;
        state.camera.target.y -= delta.y / state.camera.zoom;
    }

    // Park the authoritative cursor outside the HEXA radius while the title
    // is active; otherwise a stale world-centre position can participate.
    if state.ui_state.title_active && me.is_some() && state.last_sent_pos.is_none() {
        let parked = Vector2::new(1_000_000.0, 1_000_000.0);
        call_reducer("set_pos", serde_json::json!([parked.x, parked.y]));
        state.last_sent_pos = Some(parked);
        state.last_sent_at = Instant::now();
    }

    // Cursor heartbeat: throttled to CURSOR_SEND_HZ and only when moved.
    if map_input_allowed && me.is_some() {
        let moved = state.last_sent_pos.is_none_or(|p| {
            (p.x - mouse_world.x).abs() > 1e-4 || (p.y - mouse_world.y).abs() > 1e-4
        });
        if moved && state.last_sent_at.elapsed() >= Duration::from_secs_f32(1.0 / CURSOR_SEND_HZ) {
            call_reducer("set_pos", serde_json::json!([mouse_world.x, mouse_world.y]));
            state.last_sent_pos = Some(mouse_world);
            state.last_sent_at = Instant::now();
        }
    }

    // Painting/erasing: left-drag (or one-finger touch-drag), not while
    // panning (SHIFT/middle/right, two-finger gesturing, or the Move tool —
    // see `panning` above) or two-finger gesturing. which
    // reducer fires depends on `ui_state.tool` — mirrors `main.rs` exactly,
    // including the `over_map_area` guard against the header/footer HUD.
    if map_input_allowed
        && !gesturing
        && over_map_area
        && matches!(state.ui_state.tool, ui::Tool::Paint | ui::Tool::Erase)
    {
        if let Some(me) = me {
            if state
                .rl
                .is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT)
                && !panning
                && !state.mouse_state_stale
            {
                let (wq, wr) = world::world_to_axial(mouse_world);
                let target = match classify(&state.tables, me, wq, wr) {
                    Paintable::OwnIsland(lq, lr) => Some((0u8, lq, lr)),
                    Paintable::Margin(q, r) => Some((1u8, q, r)),
                    Paintable::Community(lq, lr) => Some((2u8, lq, lr)),
                    Paintable::Anywhere(q, r) => Some((3u8, q, r)),
                    Paintable::None => None,
                };
                if let Some(key) = target {
                    let fresh_cell = state.stroke_last != Some(key);
                    let rate_ok = state.last_paint_at.elapsed()
                        >= Duration::from_secs_f32(1.0 / CLIENT_PAINT_HZ);
                    if fresh_cell && rate_ok {
                        match (key, state.ui_state.tool == ui::Tool::Erase) {
                            ((0, lq, lr), false) => {
                                call_reducer("paint_island_cell", serde_json::json!([lq, lr]))
                            }
                            ((0, lq, lr), true) => {
                                call_reducer("erase_island_cell", serde_json::json!([lq, lr]))
                            }
                            ((1, q, r), false) => {
                                call_reducer("paint_margin_cell", serde_json::json!([q, r]))
                            }
                            ((1, q, r), true) => {
                                call_reducer("erase_margin_cell", serde_json::json!([q, r]))
                            }
                            ((2, lq, lr), false) => {
                                call_reducer("paint_community_cell", serde_json::json!([lq, lr]))
                            }
                            ((2, lq, lr), true) => {
                                call_reducer("erase_community_cell", serde_json::json!([lq, lr]))
                            }
                            ((3, q, r), false) => {
                                call_reducer("admin_paint_cell", serde_json::json!([q, r]))
                            }
                            ((3, q, r), true) => {
                                call_reducer("admin_erase_cell", serde_json::json!([q, r]))
                            }
                            _ => unreachable!(),
                        }
                        state.stroke_last = Some(key);
                        state.last_paint_at = Instant::now();
                    }
                }
            }
        }
    }

    // Island export mirrors the admin edit tool: a persistent, explicit map
    // mode whose next click chooses the target island instead of painting.
    if map_input_allowed
        && !gesturing
        && over_map_area
        && state.ui_state.tool == ui::Tool::IslandExport
        && state
            .rl
            .is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT)
    {
        let (wq, wr) = world::world_to_axial(mouse_world);
        if let Some((island_id, _, _)) = island_at(&state.tables, wq, wr) {
            if let Some(island) = state.tables.islands.get(&island_id) {
                let owner_label = player_label(&state.tables, &island.owner_hex);
                state
                    .ui_state
                    .open_island_export(ui::ExportTarget::Island(island_id), owner_label);
            }
        } else {
            state
                .ui_state
                .open_island_export(ui::ExportTarget::World, "the whole world".to_string());
        }
    }
    if state
        .rl
        .is_mouse_button_released(MouseButton::MOUSE_BUTTON_LEFT)
    {
        state.stroke_last = None;
    }

    // One-shot click/tap eyedropper for mobile and trackpads. Middle-click
    // remains available as the desktop shortcut below.
    if map_input_allowed
        && !gesturing
        && over_map_area
        && state.ui_state.tool == ui::Tool::Eyedropper
        && state
            .rl
            .is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT)
    {
        if let Some(me) = me {
            let (wq, wr) = world::world_to_axial(mouse_world);
            if pick_color_at(
                &state.tables,
                &mut state.ui_state,
                me,
                wq,
                wr,
            ) {
                if state.ui_state.finish_eyedropper() {
                    call_reducer("set_lock", serde_json::json!([false]));
                }
            }
        }
    }

    // Mirrors `main.rs`'s AdminEdit click
    // block — a persistent, mobile-friendly tool state (not a right-click/
    // long-press) whose only effect is opening the admin edit modal on tap.
    if map_input_allowed
        && !gesturing
        && over_map_area
        && state.ui_state.tool == ui::Tool::AdminEdit
        && state
            .rl
            .is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT)
    {
        if let Some(me) = me {
            let (wq, wr) = world::world_to_axial(mouse_world);
            if let Some((island_id, _, _)) = island_at(&state.tables, wq, wr) {
                let editable = state.tables.islands.get(&island_id).is_some_and(|isl| {
                    isl.owner_hex != me && isl.owner_hex != world::COMMUNITY_OWNER_HEX
                });
                if editable {
                    open_admin_edit(state, island_id);
                }
            }
        }
    }

    // Middle-click shortcut for the eyedropper. The footer tool above is
    // the click/tap path used by touch devices and trackpads.
    if !map_input_allowed {
        state.middle_click = None;
    } else {
        if state
            .rl
            .is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_MIDDLE)
            && over_map_area
        {
            state.middle_click = Some(mouse_screen);
        }
        if state
            .rl
            .is_mouse_button_released(MouseButton::MOUSE_BUTTON_MIDDLE)
        {
            if let Some(press) = state.middle_click.take() {
                let dx = mouse_screen.x - press.x;
                let dy = mouse_screen.y - press.y;
                if (dx * dx + dy * dy).sqrt() <= MIDDLE_CLICK_TOL_PX {
                    if let Some(me) = me {
                        let (wq, wr) = world::world_to_axial(mouse_world);
                        if pick_color_at(
                            &state.tables,
                            &mut state.ui_state,
                            me,
                            wq,
                            wr,
                        ) {
                            if state.ui_state.finish_eyedropper() {
                                call_reducer("set_lock", serde_json::json!([false]));
                            }
                        }
                    }
                }
            }
        }
    }

    // Long-press-to-merge: hold steady on a painted cell for
    // LONG_PRESS_HOLD -> merge_with_cell. Mirrors `main.rs` exactly,
    // including the deferred info-popup / double-click-to-like handling
    // below (see its comment there for why the popup-open is delayed).
    if !map_input_allowed || gesturing {
        state.long_press = None;
    } else if let Some(me) = me {
        // `AdminEdit` is handled entirely by its own
        // block above — mirrors `main.rs`'s exclusion here.
        // `Eyedropper` arms this too: sampling a hue you don't own is not an
        // error, it's an invitation to hold and merge for it.
        if state.ui_state.tool != ui::Tool::AdminEdit
            && state.ui_state.tool != ui::Tool::IslandExport
            && !panning
            && over_map_area
            && state
                .rl
                .is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT)
        {
            let (wq, wr) = world::world_to_axial(mouse_world);
            state.long_press = Some(LongPress {
                press_screen: mouse_screen,
                press_at: Instant::now(),
                target: merge_target_at(&state.tables, wq, wr)
                    .filter(|&(_, _, hue)| !have_hue(&state.tables, me, hue)),
                // Anywhere on a FOREIGN island's
                // territory, not just its center. Mirrors `main.rs`; own
                // island's popup now opens via the "My Isle" footer button.
                // Author reversal: the community island is excluded here
                // — no info popup, no like, matching the hover exclusion
                // below. Its sentinel owner would otherwise pass this
                // `owner_hex != me` check like any other foreign island.
                // The eyedropper's own tap consumes the press, so it must not
                // also queue a popup / double-click like.
                info_target: island_at(&state.tables, wq, wr)
                    .filter(|_| state.ui_state.tool != ui::Tool::Eyedropper)
                    .filter(|&(id, _, _)| {
                        state.tables.islands.get(&id).is_some_and(|isl| {
                            isl.owner_hex != me && isl.owner_hex != world::COMMUNITY_OWNER_HEX
                        })
                    })
                    .map(|(id, _, _)| id),
                fired: false,
            });
        }
        if let Some(lp) = &mut state.long_press {
            let dx = mouse_screen.x - lp.press_screen.x;
            let dy = mouse_screen.y - lp.press_screen.y;
            let moved = (dx * dx + dy * dy).sqrt() > LONG_PRESS_TOL_PX;
            let released = !state
                .rl
                .is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT);
            if moved || released {
                if released && !moved && !lp.fired {
                    if let Some(island_id) = lp.info_target {
                        let is_double = state.pending_info_click.is_some_and(|(t, pos, id)| {
                            let ddx = pos.x - mouse_screen.x;
                            let ddy = pos.y - mouse_screen.y;
                            id == island_id
                                && t.elapsed() < DOUBLE_CLICK_WINDOW
                                && (ddx * ddx + ddy * ddy).sqrt() <= DOUBLE_CLICK_TOL_PX
                        });
                        if is_double {
                            let liked = already_liked(&state.tables, island_id, me);
                            if liked {
                                call_reducer("unlike_island", serde_json::json!([island_id]));
                            } else {
                                call_reducer("like_island", serde_json::json!([island_id]));
                            }
                            state.ui_state.spawn_like_anim(mouse_screen, !liked);
                            state.pending_info_click = None;
                        } else {
                            state.pending_info_click =
                                Some((Instant::now(), mouse_screen, island_id));
                        }
                    }
                }
                state.long_press = None;
            } else if !lp.fired && lp.press_at.elapsed() >= LONG_PRESS_HOLD {
                lp.fired = true;
                if let Some((kind, id, _)) = lp.target {
                    call_reducer("merge_with_cell", serde_json::json!([kind, id]));
                }
            }
        }
    } else {
        state.long_press = None;
    }
    // Resolves a `pending_info_click` into an actual popup-open once
    // DOUBLE_CLICK_WINDOW has passed without a follow-up click landing on
    // the same island (which would have consumed it as a like/unlike toggle
    // instead, above). Mirrors `main.rs` exactly.
    if let Some((clicked_at, _, island_id)) = state.pending_info_click {
        if clicked_at.elapsed() >= DOUBLE_CLICK_WINDOW {
            open_island_info(state, island_id);
            // Follow-up (author-requested): mirrors `main.rs` —
            // the hover tooltip that replaced this click's old
            // popup-opening role is non-interactive, so a resolved single
            // click on a foreign island now directly opens its itch.io link
            // (if set) too — also decision 17's "tap opens" path for touch,
            // which has no hover.
            if let Some(rate_id) = state
                .tables
                .islands
                .get(&island_id)
                .and_then(|isl| isl.itch_rate_id)
            {
                let url = format!("https://itch.io/jam/raylib-6x-gamejam/rate/{rate_id}");
                let js_url = serde_json::to_string(&url).unwrap();
                run_js(&format!("window.open({js_url}, '_blank')"));
                call_reducer("click_link", serde_json::json!([island_id]));
            }
            state.pending_info_click = None;
        }
    }

    // Decision 17: island info on hover (desktop). Mirrors
    // `main.rs` — purely position-based, independent of the click/long-press
    // gesture block above (left untouched for double-click-to-like and
    // touch; raylib-web aliases a single touch to ordinary mouse events, so
    // that path already covers touch taps). Excludes the
    // header/footer bands, whose screen coordinates still map to SOME world
    // tile via the camera transform — see `over_map_area` (computed above)
    // for why (a footer button click could otherwise have the hover logic
    // overwrite a just-opened own-island popup).
    let currently_hovered_foreign = over_map_area
        .then(|| {
            me.and_then(|me| {
                let (wq, wr) = world::world_to_axial(mouse_world);
                island_at(&state.tables, wq, wr)
                    .filter(|&(id, _, _)| {
                        state.tables.islands.get(&id).is_some_and(|isl| {
                            isl.owner_hex != me && isl.owner_hex != world::COMMUNITY_OWNER_HEX
                        })
                    })
                    .map(|(id, _, _)| id)
            })
        })
        .flatten();
    match currently_hovered_foreign {
        Some(id) => {
            if state.hover_target.map(|(hid, _)| hid) != Some(id) {
                state.hover_target = Some((id, Instant::now()));
            }
            // Suppressed while any button/gesture is active (mid paint
            // stroke, pan, pinch, or long-press).
            let gesturing_input = state
                .rl
                .is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT)
                || state
                    .rl
                    .is_mouse_button_down(MouseButton::MOUSE_BUTTON_MIDDLE)
                || gesturing;
            // `any_modal_open` covers My Isle and the Escape/help overlay
            // too — without it, hovering a foreign island would, after the
            // delay, silently overwrite/close whichever modal was open
            // (mirrors main.rs).
            if !state.ui_state.any_modal_open() && !gesturing_input {
                if let Some((hid, since)) = state.hover_target {
                    let already_open = state
                        .ui_state
                        .island_popup
                        .as_ref()
                        .is_some_and(|p| p.island_id == id);
                    if hid == id && !already_open && since.elapsed() >= HOVER_OPEN_DELAY {
                        open_island_info(state, id);
                    }
                }
            }
        }
        None => state.hover_target = None,
    }
    // Closes on hover-out, regardless of how the popup was opened (hover or
    // the click/double-click path above). Mirrors `main.rs` — own-island
    // popups are exempt (never a hover target, so this would otherwise slam
    // them shut the frame after opening).
    if let Some(popup) = &state.ui_state.island_popup {
        if !popup.is_own && currently_hovered_foreign != Some(popup.island_id) {
            state.ui_state.island_popup = None;
        }
    }

    // View-space culling bounds, padded well past the screen edges.
    let pad = world::constants::VIEW_CULL_PAD;
    let top_left = state
        .rl
        .get_screen_to_world2D(Vector2::new(0.0, 0.0), state.camera);
    let bottom_right = state
        .rl
        .get_screen_to_world2D(Vector2::new(720.0, 720.0), state.camera);
    let (view_min_x, view_max_x) = (top_left.x - pad, bottom_right.x + pad);
    let (view_min_y, view_max_y) = (top_left.y - pad, bottom_right.y + pad);
    let in_view = |p: Vector2| {
        p.x >= view_min_x && p.x <= view_max_x && p.y >= view_min_y && p.y <= view_max_y
    };

    // Mirrors `main.rs`'s `seed_hues`: each island's border is drawn in its
    // owner's SEED hue (their starting color, or reset_account's reseed —
    // exactly one `obtained_with_hex.is_none()` row exists per owner at any
    // time), not the live/nudged brush, so the world map is browsable by
    // "whose island is that" at a glance.
    let seed_hues: HashMap<&str, u16> = state
        .tables
        .inventory
        .values()
        .filter(|inv| inv.obtained_with_hex.is_none())
        .map(|inv| (inv.owner_hex.as_str(), inv.hue))
        .collect();

    // Mirrors `main.rs` — shrinks other players' cursors with
    // camera zoom (relative to their fixed size at the default
    // `ISLAND_FIT_ZOOM`), floored so they stay findable when zoomed out.
    let other_cursor_scale =
        (state.camera.zoom / ISLAND_FIT_ZOOM).max(world::constants::CURSOR_MIN_SCALE);

    // Mirrors `main.rs` — the server-authoritative central
    // formation, with each member placed at ITS OWN
    // `vertex_index` (paired per-row, not by list position). `me`'s own row
    // is included like everyone else's: the LOCAL player's
    // own cursor should visibly move to its hexagon slot too (see the
    // local-cursor draw call below).
    let hexa_rows: Vec<(&String, &HexaClusterRow)> = state
        .tables
        .hexa_clusters
        .iter()
        .filter(|_| !replay_mode)
        .collect();
    // Flattened (key, hexagon target, raw fallback) triples across every
    // cluster, fed straight to `hexa_advance_display` — mirrors `main.rs` via
    // the shared `world::hexa_cluster_frame`; only the row shape and the user-table
    // lookup closure below are web-specific.
    let mut cluster_members: Vec<(String, Vector2, Vector2)> = Vec::new();
    // Fixed world-centre target per snapped member, for the rotated
    // cursor draw — mirrors `main.rs`'s `hexa_centres`.
    let mut hexa_centres: HashMap<String, Vector2> = HashMap::new();
    // World-space hexagon edges (drawn inside `d2` below, mirrors `main.rs`).
    let mut hexa_polygons: Vec<(Vec<Vector2>, bool)> = Vec::new();
    if let Some(&(_, first)) = hexa_rows.first() {
        let members: Vec<(String, u32)> = hexa_rows
            .iter()
            .map(|&(id, row)| (id.clone(), row.vertex_index))
            .collect();
        let centre = Vector2::zero();
        let (frame, vertices) = world::hexa_cluster_frame(
            centre,
            first.member_count as usize,
            &members,
            |key, target| {
                if Some(key.as_str()) == me {
                    mouse_world
                } else {
                    state
                        .tables
                        .users
                        .get(key)
                        .map(|u| Vector2::new(u.cx, u.cy))
                        .unwrap_or(target)
                }
            },
        );
        for (key, vertex_index) in &members {
            let i = *vertex_index as usize % 6;
            let a = vertices[i];
            let b = vertices[(i + 1) % 6];
            hexa_centres.insert(
                key.clone(),
                Vector2::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5),
            );
        }
        cluster_members.extend(frame);
        hexa_polygons.push((vertices, first.ignited));
    }
    state.hexa_display = world::hexa_advance_display(
        &state.hexa_display,
        &cluster_members,
        state.rl.get_frame_time(),
    );

    let other_cursors: Vec<(Vector2, Color, bool, String, Option<Vector2>)> = state
        .tables
        .users
        .iter()
        .filter(|_| !replay_mode)
        // Mirrors `main.rs` — cursors used to vanish ~3s
        // after a player stopped moving; a stationary-but-connected player
        // should stay visible the whole time they're online.
        .filter(|(id, u)| u.online && Some(id.as_str()) != me)
        .map(|(id, u)| {
            // Mirrors `main.rs` — a hexagon-cluster member renders at
            // its lerped snapped display position instead of its raw
            // cursor position, and carries its cluster's centre
            // (screen-projected) for the rotated arrow draw.
            let world_pos = state
                .hexa_display
                .get(id)
                .copied()
                .unwrap_or(Vector2::new(u.cx, u.cy));
            (
                state.rl.get_world_to_screen2D(world_pos, state.camera),
                world::hsv_color(u.hue, u.sat, u.val),
                u.locked,
                u.name.clone().unwrap_or_default(),
                hexa_centres
                    .get(id)
                    .map(|&c| state.rl.get_world_to_screen2D(c, state.camera)),
            )
        })
        .collect();

    // Mirrors `main.rs`'s `my_hexa_screen` — the
    // LOCAL player's own cursor also renders at its lerped hexagon-snap
    // position while participating. `None` (not clustered, or `me` unknown
    // yet) falls back to the literal mouse position at the draw call.
    // Carries (tip, cluster centre), both screen-space, for the rotated
    // snapped-arrow draw.
    let my_hexa_screen: Option<(Vector2, Vector2)> = (!replay_mode)
        .then_some(())
        .and_then(|_| me)
        .and_then(|me| {
            let pos = state.hexa_display.get(me)?;
            let centre = hexa_centres.get(me)?;
            Some((
                state.rl.get_world_to_screen2D(*pos, state.camera),
                state.rl.get_world_to_screen2D(*centre, state.camera),
            ))
        });
    let eyedropper_preview = (state.ui_state.tool == ui::Tool::Eyedropper)
        .then(|| {
            let (q, r) = world::world_to_axial(mouse_world);
            painted_color_at(&state.tables, q, r).map(|(h, s, v)| world::hsv_color(h, s, v))
        })
        .flatten()
        .unwrap_or(Color::new(150, 150, 156, 255));

    let camera = state.camera;
    // Mirrors `main.rs` — skip the per-tile outline pass past
    // this zoom (visual noise at that size; a small render win too).
    let show_tile_outline = camera.zoom >= BORDERLESS_ZOOM_THRESHOLD;
    // Grabbed before `begin_drawing` hands out its mutable borrow — mirrors
    // `main.rs`.
    let fps = state.rl.get_fps();
    let recovered_history = state.recovered_replay.as_ref();
    let island_motion = state
        .replay_clock
        .as_ref()
        .is_none_or(replay::ReplayClock::island_motion);
    let hover_takeable;
    let mut d = state.rl.begin_drawing(&state.thread);
    d.clear_background(Color::new(18, 18, 24, 255));

    // Mirrors main.rs: the baked map stands in for the live hexes on the
    // title screen and through the zoomed-out part of the launch intro,
    // crossfading to real cells as the camera closes on the island.
    let map_alpha = if replay_mode {
        0.0
    } else if state.ui_state.title_active {
        1.0
    } else {
        title_map::blend_alpha(camera.zoom)
    };
    let skip_live_cells = map_alpha >= 1.0;
    let title_map_view = state.title_map_view;
    let title_map_baked_at = state.title_map_baked_at;

    {
        let mut d2 = d.begin_mode2D(camera);

        // Drawn through the camera so it tracks the intro's pan/zoom.
        if map_alpha > 0.0 {
            if let Some(texture) = state.title_map.as_ref() {
                d2.draw_texture_pro(
                    texture,
                    Rectangle::new(0.0, 0.0, texture.width as f32, texture.height as f32),
                    title_map::world_rect_for(title_map_view),
                    Vector2::zero(),
                    0.0,
                    Color::new(255, 255, 255, (map_alpha * 255.0) as u8),
                );
            }
        }

        // Mirrors main.rs: anything painted since the bake, drawn live over
        // it, so a player who paints and zooms back out still sees their own
        // work on an image that never refreshes at runtime.
        if skip_live_cells {
            let baked_at = title_map_baked_at;
            for cell in state.tables.island_cells.values() {
                if cell.painted_at_micros <= baked_at {
                    continue;
                }
                let Some(island) = state.tables.islands.get(&cell.island_id) else {
                    continue;
                };
                let (q, r) = world::slot_coords(island.slot);
                let (fcx, fcy) = world::slot_center(q, r);
                let (h, s, v) = world::unpack_hsv(cell.color);
                world::draw_hex(
                    &mut d2,
                    world::axial_to_world(fcx + cell.q, fcy + cell.r),
                    1.0,
                    world::hsv_color(h, s, v),
                    None,
                );
            }
            for cell in state.tables.margin_cells.values() {
                if cell.painted_at_micros <= baked_at {
                    continue;
                }
                let (h, s, v) = world::unpack_hsv(cell.color);
                world::draw_hex(
                    &mut d2,
                    world::axial_to_world(cell.q, cell.r),
                    1.0,
                    world::hsv_color(h, s, v),
                    None,
                );
            }
        }

        if recovered_history.is_none() && !skip_live_cells {
            for (&island_id, island) in &state.tables.islands {
                if recovered_history.is_none()
                    && state
                        .replay_clock
                        .as_ref()
                        .is_some_and(|clock| !clock.is_visible(island.created_at_micros))
                {
                    continue;
                }
                let (q, r) = world::slot_coords(island.slot);
                let (fcx, fcy) = world::slot_center(q, r);
                let center = world::axial_to_world(fcx, fcy);
                if !replay_mode && !in_view(center) {
                    continue;
                }
                let mine = me == Some(island.owner_hex.as_str());
                let unpainted_fill =
                    world::unpainted_island_fill(island.owner_hex == world::COMMUNITY_OWNER_HEX);
                // Point-lookup each rendered cell by its
                // packed id in the already-id-keyed `island_cells` map instead of
                // collecting a fresh (island_id, q, r) -> color HashMap from
                // EVERY island_cell row in the world every frame — cost is now
                // proportional to in-view cells, not total painted cells.
                for &(dq, dr) in world::island_offsets() {
                    let cell_world = world::axial_to_world(fcx + dq, fcy + dr);
                    let id = world::island_cell_id(island_id, dq, dr);
                    let fill = state
                        .tables
                        .island_cells
                        .get(&id)
                        .filter(|c| {
                            state
                                .replay_clock
                                .as_ref()
                                .is_none_or(|clock| clock.is_visible(c.painted_at_micros))
                        })
                        .map(|c| {
                            let (h, s, v) = world::unpack_hsv(c.color);
                            world::hsv_color(h, s, v)
                        })
                        .or(unpainted_fill);
                    let Some(fill) = fill else {
                        continue;
                    };
                    world::draw_hex(
                        &mut d2,
                        cell_world,
                        1.0,
                        fill,
                        show_tile_outline.then_some(Color::new(40, 40, 46, 255)),
                    );
                }
                // Mirrors `main.rs` — sat/val is now
                // `START_SAT`/`START_VAL` exactly (was a fixed 85/95 lookalike
                // shade).
                //
                // Mirrors `main.rs` — an owner-set
                // `border_color`/`border_hidden` override takes priority over
                // the seed-hue default.
                let border_color = if island.border_hidden && !replay_mode {
                    None
                } else if let Some(packed) = island.border_color {
                    let (h, s, v) = world::unpack_hsv(packed);
                    Some(world::hsv_color(h, s, v))
                } else {
                    seed_hues
                        .get(island.owner_hex.as_str())
                        .map(|&hue| world::hsv_color(hue, START_SAT, START_VAL))
                        .or_else(|| replay_mode.then_some(Color::new(225, 225, 232, 255)))
                };
                if let Some(border_color) = border_color {
                    let r_f = ISLAND_RADIUS as f32;
                    let corners: Vec<Vector2> = world::DIRECTIONS
                        .iter()
                        .map(|&(dq, dr)| {
                            world::axial_to_world(
                                fcx + (dq as f32 * r_f) as i32,
                                fcy + (dr as f32 * r_f) as i32,
                            )
                        })
                        .collect();
                    // Own island's border is thicker — still colored by
                    // identity like every other island, just easier to spot.
                    // Mirrors `main.rs` — thickness is now a
                    // constant SCREEN pixel width (divided by zoom to convert
                    // back to world units), not a fixed world-unit value, so it
                    // no longer shrinks under a pixel (and randomly vanishes on
                    // whichever edge rounds down first) at low zoom.
                    let px = if mine { 3.0 } else { 1.5 };
                    let thickness = px / camera.zoom;
                    for i in 0..6 {
                        d2.draw_line_ex(corners[i], corners[(i + 1) % 6], thickness, border_color);
                    }
                }
            }
        } else if let Some(history) = recovered_history {
            // The recovered stream carries island creation, deletion and slot
            // re-ranks. Draw the bases from that state rather than from the
            // current subscription snapshot, which may be a different layout.
            for &island_id in history.island_slots.keys() {
                let Some(slot) = history.display_slot(island_id, island_motion) else {
                    continue;
                };
                let (q, r) = world::slot_coords(slot);
                let (fcx, fcy) = world::slot_center(q, r);
                let center = world::axial_to_world(fcx, fcy);
                let island = state.tables.islands.get(&island_id);
                let mine = island.is_some_and(|row| me == Some(row.owner_hex.as_str()));
                let fill = island.and_then(|row| {
                    world::unpainted_island_fill(row.owner_hex == world::COMMUNITY_OWNER_HEX)
                });
                if let Some(fill) = fill {
                    world::draw_hex(&mut d2, center, ISLAND_RADIUS as f32, fill, None);
                }

                // Replay deliberately ignores `border_hidden`; it is a
                // presentation control for live play, not a history filter.
                //
                // The historical color (baked in by the extractor at each
                // island-insert event, see `RECOVERED_HISTORY_MAGIC`'s v3
                // comment) is tried FIRST, not just as a fallback: it's the
                // color that island's border actually had at this point in
                // the scrub position, whereas the live-table lookups below
                // reflect only the CURRENT database — which has nothing left
                // to look up once an island has since been deleted
                // (`admin_delete_island`/`delete_account` remove the owner's
                // `Inventory` rows too). Live lookups stay as a fallback for
                // the sentinel case (extractor couldn't resolve it either).
                let border_color = history
                    .island_border
                    .get(&island_id)
                    .copied()
                    .filter(|&packed| packed != UNKNOWN_BORDER_COLOR)
                    .map(|packed| {
                        let (h, s, v) = world::unpack_hsv(packed);
                        world::hsv_color(h, s, v)
                    })
                    .or_else(|| {
                        island.and_then(|row| {
                            row.border_color.map(|packed| {
                                let (h, s, v) = world::unpack_hsv(packed);
                                world::hsv_color(h, s, v)
                            })
                        })
                    })
                    .or_else(|| {
                        island.and_then(|row| {
                            seed_hues
                                .get(row.owner_hex.as_str())
                                .map(|&hue| world::hsv_color(hue, START_SAT, START_VAL))
                        })
                    })
                    .unwrap_or(Color::new(225, 225, 232, 255));
                let r_f = ISLAND_RADIUS as f32;
                let corners: Vec<Vector2> = world::DIRECTIONS
                    .iter()
                    .map(|&(dq, dr)| {
                        world::axial_to_world(
                            fcx + (dq as f32 * r_f) as i32,
                            fcy + (dr as f32 * r_f) as i32,
                        )
                    })
                    .collect();
                let thickness = if mine { 3.0 } else { 1.5 } / camera.zoom;
                for i in 0..6 {
                    d2.draw_line_ex(corners[i], corners[(i + 1) % 6], thickness, border_color);
                }
            }
        }

        if let Some(history) = recovered_history {
            for (&(island_id, q, r), &color) in &history.island_cells {
                let Some(slot) = history.display_slot(island_id, island_motion) else {
                    // The cell belongs to an island that is not present at
                    // this point in history (for example, during the atomic
                    // delete transaction), so it has no world position.
                    continue;
                };
                let (slot_q, slot_r) = world::slot_coords(slot);
                let (center_q, center_r) = world::slot_center(slot_q, slot_r);
                let (h, s, v) = world::unpack_hsv(color);
                world::draw_hex(
                    &mut d2,
                    world::axial_to_world(center_q + q, center_r + r),
                    1.0,
                    world::hsv_color(h, s, v),
                    show_tile_outline.then_some(Color::new(40, 40, 46, 255)),
                );
            }
            for (&(q, r), &color) in &history.margin_cells {
                let (h, s, v) = world::unpack_hsv(color);
                world::draw_hex(
                    &mut d2,
                    world::axial_to_world(q, r),
                    1.0,
                    world::hsv_color(h, s, v),
                    show_tile_outline.then_some(Color::new(30, 30, 34, 255)),
                );
            }
        } else {
            for cell in state.tables.margin_cells.values() {
                if skip_live_cells {
                    break; // covered by the baked map, same as the islands above
                }
                if state
                    .replay_clock
                    .as_ref()
                    .is_some_and(|clock| !clock.is_visible(cell.painted_at_micros))
                {
                    continue;
                }
                let p = world::axial_to_world(cell.q, cell.r);
                if !replay_mode && !in_view(p) {
                    continue;
                }
                let (h, s, v) = world::unpack_hsv(cell.color);
                world::draw_hex(
                    &mut d2,
                    p,
                    1.0,
                    world::hsv_color(h, s, v),
                    show_tile_outline.then_some(Color::new(30, 30, 34, 255)),
                );
            }
        }

        // World-space, mirrors `main.rs`.
        if !replay_mode {
            if let Some((_, pos, elapsed)) = active_gift {
                world::draw_gift_icon(&mut d2, pos, elapsed);
            }
        }

        // World-space hexagon edges, mirrors `main.rs`.
        for (vertices, ignited) in &hexa_polygons {
            world::draw_hexa_polygon(&mut d2, vertices, *ignited);
        }

        let (hq, hr) = world::world_to_axial(mouse_world);
        // `over_map_area`: mirrors `main.rs` — without it, hovering a footer
        // button could flash the white hex through the HUD's semi-
        // transparent background.
        let hover_paintable = map_input_allowed
            && over_map_area
            && matches!(state.ui_state.tool, ui::Tool::Paint | ui::Tool::Erase)
            && me.is_some_and(|me| !matches!(classify(&state.tables, me, hq, hr), Paintable::None));
        if hover_paintable {
            let hover_center = world::axial_to_world(hq, hr);
            d2.draw_poly(hover_center, 6, 1.0, 0.0, Color::new(255, 255, 255, 70));
            // Mirrors `main.rs`: constant screen pixel width like the island
            // border, so it stays visible zoomed all the way out.
            d2.draw_poly_lines_ex(
                hover_center,
                6,
                1.0,
                0.0,
                HOVER_BORDER_PX / camera.zoom,
                Color::new(255, 255, 255, 210),
            );
        }

        hover_takeable = map_input_allowed
            && over_map_area
            && me.is_some_and(|me| {
                matches!(classify(&state.tables, me, hq, hr), Paintable::None)
                    && merge_target_at(&state.tables, hq, hr)
                        .is_some_and(|(_, _, hue)| !have_hue(&state.tables, me, hue))
            });
    }

    let hexa_cursor_scale = camera.zoom / ISLAND_FIT_ZOOM;
    if export_subject.is_none() {
        for (screen, color, locked, name, snap_centre) in &other_cursors {
            match snap_centre {
                // Follow-up #2: mirrors `main.rs` — a hexagon-snapped
                // cursor aims its tip at the cluster centre.
                Some(centre) => world::draw_cursor_snapped(
                    &mut d,
                    *screen,
                    *centre,
                    *color,
                    hexa_cursor_scale,
                    *locked,
                ),
                None => {
                    world::draw_cursor_scaled(&mut d, *screen, *color, other_cursor_scale, *locked)
                }
            }
            if snap_centre.is_none() && other_cursor_scale >= 0.5 {
                world::draw_cursor_label(&mut d, *screen, name, other_cursor_scale);
            }
        }
    }

    let own_brush = me
        .and_then(|me| state.tables.users.get(me))
        .map(|u| ((u.hue, u.sat, u.val), u.xp, u.locked));
    if replay_mode {
        if let Some(clock) = state.replay_clock.as_ref() {
            if let Some(history) = state.recovered_replay.as_ref() {
                if !state.gif_recording && !state.clean_timeline_export {
                    replay::draw_history_overlay(
                        &mut d,
                        clock,
                        history.visible_tiles(),
                        history.applied_playback_events(),
                        history.playback_events(),
                    );
                }
            } else {
                let visible_tiles = state
                    .tables
                    .island_cells
                    .values()
                    .filter(|cell| clock.is_visible(cell.painted_at_micros))
                    .count()
                    + state
                        .tables
                        .margin_cells
                        .values()
                        .filter(|cell| clock.is_visible(cell.painted_at_micros))
                        .count();
                // Events inside the replay window, not every tile in the
                // world — otherwise `Cut` visibly changes nothing.
                let total_tiles = state
                    .tables
                    .island_cells
                    .values()
                    .filter(|cell| clock.contains(cell.painted_at_micros))
                    .count()
                    + state
                        .tables
                        .margin_cells
                        .values()
                        .filter(|cell| clock.contains(cell.painted_at_micros))
                        .count();
                replay::draw_overlay(&mut d, clock, visible_tiles, total_tiles);
            }
        }
    }
    if let (Some(me), Some((brush, xp, locked))) = (me, own_brush) {
        let hues: Vec<u16> = state
            .tables
            .inventory
            .values()
            .filter(|i| i.owner_hex == me)
            .map(|i| i.hue)
            .collect();
        let level = world::level_of(xp);
        let short_id = me.to_string();
        let rerank_secs = state
            .tables
            .configs
            .get(&0)
            .and_then(|c| c.next_rerank_at_micros)
            .map(|t| t - state.now_micros)
            .filter(|&d| d > 0)
            .map(|d| (d as f64 / 1_000_000.0).ceil() as i64);
        let info = ui::HudInfo {
            short_id: short_hex(&short_id),
            level,
            xp,
            camera_target: state.camera.target,
            online,
            total,
            locked,
            brush,
            sat_cap: world::sat_cap(level),
            hues: &hues,
            show_token_import: true,
            rerank_secs,
            link_id: my_island(&state.tables, me).and_then(|(isl, _)| isl.itch_rate_id),
            is_admin: is_admin(&state.tables, me),
        };
        if !replay_mode {
            if let Some(subject) = export_subject.as_ref() {
                ui::draw_export_frame(&mut d, subject);
            } else {
                ui::draw(&mut d, &state.ui_state, &info, mouse_screen);

                // Mirrors `main.rs` — eraser mode draws the cursor in a
                // neutral gray plus a small eraser badge instead of the brush hue.
                let (hue, sat, val) = brush;
                let cursor_color = if state.ui_state.tool == ui::Tool::Erase {
                    Color::new(210, 210, 216, 255)
                } else {
                    world::hsv_color(hue, sat, val)
                };
                // Mirrors `main.rs` — visually snaps to the
                // hexagon slot while merging; painting/hover logic still uses the
                // real `mouse_world`/`mouse_screen`, only this draw call moves
                // (and, follow-up #2, rotates to aim at the cluster centre).
                if state.ui_state.tool == ui::Tool::Eyedropper {
                    ui::draw_eyedropper_cursor(&mut d, mouse_screen, eyedropper_preview);
                } else if state.ui_state.tool == ui::Tool::IslandExport {
                    ui::draw_export_cursor(&mut d, mouse_screen);
                } else {
                    match my_hexa_screen {
                        Some((tip, centre)) => world::draw_cursor_snapped(
                            &mut d,
                            tip,
                            centre,
                            cursor_color,
                            hexa_cursor_scale,
                            locked,
                        ),
                        None => world::draw_cursor(&mut d, mouse_screen, cursor_color, locked),
                    }
                }
            }
        }
    }
    // Identity arrives over the same socket as the initial subscription,
    // so draw the title directly until the normal HUD branch can do it.
    if state.ui_state.title_active && me.is_none() && !replay_mode {
        ui::draw_title(&mut d, &state.ui_state, mouse_screen);
    }
    if export_subject.is_none() && !replay_mode {
        if state.ui_state.tool == ui::Tool::Erase {
            world::draw_eraser_badge(&mut d, mouse_screen);
        } else if hover_takeable
            && matches!(
                state.ui_state.tool,
                ui::Tool::Paint | ui::Tool::Erase | ui::Tool::Eyedropper
            )
        {
            // Move tool: left-drag pans instead of merging, so the "+"
            // take-hint (which promises a long-press merge) would mislead.
            world::draw_plus_hint(&mut d, mouse_screen);
        }
        if let Some(frac) = state
            .long_press
            .as_ref()
            .filter(|lp| !lp.fired && lp.target.is_some())
            .map(|lp| {
                (lp.press_at.elapsed().as_secs_f32() / LONG_PRESS_HOLD.as_secs_f32())
                    .clamp(0.0, 1.0)
            })
        {
            world::draw_hold_ring(&mut d, mouse_screen, frac);
        }
        // Hidden on the title screen, colored to match the header's grey —
        // mirrors `main.rs`.
        if !state.ui_state.title_active {
            world::draw_fps_grey(&mut d, 8, 32, fps);
        }
    }
    // The operating-system/browser cursor is hidden because normal play
    // draws a brush-coloured pointer. Replay suppresses painting UI, so keep
    // a neutral pointer visible for its controls instead of leaving users
    // without a cursor.
    if export_subject.is_none() && replay_mode && !state.gif_recording && !state.clean_timeline_export
    {
        world::draw_cursor(&mut d, mouse_screen, Color::RAYWHITE, false);
    }
    // EndDrawing must complete before the browser reads the finished canvas.
    drop(d);
    if state.gif_recording {
        state.gif_frame_counter += 1;
        if state
            .gif_frame_counter
            .is_multiple_of(GIF_CAPTURE_EVERY_FRAMES)
            || finish_gif_recording
        {
            let mut image = state.rl.load_image_from_screen(&state.thread);
            image.resize_nn(GIF_EXPORT_SIZE, GIF_EXPORT_SIZE);
            unsafe {
                hexel_gif_frame(image.data().cast(), GIF_FRAME_DELAY_CS, GIF_EXPORT_SIZE * 4);
            }
        }
    }
    if finish_gif_recording {
        let mut length = 0_usize;
        let data = unsafe { hexel_gif_end(&mut length) };
        state.gif_recording = false;
        if !data.is_null() && length > 0 {
            let scope = if state.gif_export_island.is_some() {
                "island"
            } else {
                "world"
            };
            run_js(&format!(
                "window.stdb && window.stdb.downloadReplayGif({}, {length}, '{scope}')",
                data as usize
            ));
            unsafe { hexel_gif_free(data.cast()) };
            state.export_saved_for = DOWNLOADED_BADGE_SECS;
            state.clean_timeline_export = false;
            state.ui_state.show_info_toast("GIF downloaded".to_string());
        } else {
            state
                .ui_state
                .show_info_toast("GIF export failed".to_string());
        }
    }
    if stop_replay_recording {
        run_js("window.stdb && window.stdb.stopReplayRecording()");
        state.clean_timeline_export = false;
        // The recorder finishes asynchronously; JS saves the blob as soon as
        // it is assembled, since the player already asked for it by clicking.
        state.export_saved_for = DOWNLOADED_BADGE_SECS;
        state.ui_state.show_info_toast("video downloaded".to_string());
    }
    if export_subject.is_some() {
        run_js("window.stdb && window.stdb.downloadIslandImage()");
        state
            .ui_state
            .show_info_toast("island image downloaded".to_string());
    }

    // Capture progress, reported to the DOM bar (see `showExportProgress` in
    // game.html for why it can't be drawn in-game). Driven off the replay
    // clock, which is what actually paces a timelapse export, so the bar
    // tracks how much of the history has been recorded.
    let capturing = state.gif_recording || state.replay_recording;
    if capturing {
        let percent = state
            .replay_clock
            .as_ref()
            .map_or(0.0, |clock| clock.progress() * 100.0)
            .round() as i32;
        if percent != state.export_progress_shown {
            state.export_progress_shown = percent;
            let kind = if state.gif_recording { "GIF" } else { "video" };
            run_js(&format!(
                "window.stdb && window.stdb.showExportProgress({percent}, 'Recording {kind} - {percent}%')"
            ));
        }
    } else if state.export_progress_shown >= 0 {
        state.export_progress_shown = -1;
        run_js("window.stdb && window.stdb.hideExportProgress()");
    }
}

fn main() {
    let (mut rl, thread) = raylib::init()
        .size(720, 720) // jam hard constraint
        .title("hexel — raylib 6.0 + SpacetimeDB (web)")
        .build();
    rl.hide_cursor(); // we draw our own pointer in the caller's brush color

    // Leaked alongside `state` below (both live for the process's
    // whole lifetime under `emscripten_set_main_loop_arg` — see its comment).
    let audio: Option<&'static RaylibAudio> = RaylibAudio::init_audio_device()
        .ok()
        .map(|a| &*Box::leak(Box::new(a)));
    let sfx = audio.map(sfx::Sfx::load);

    let title_map = title_map::load(&mut rl, &thread);
    let title_map_view = title_map::view();
    let title_map_baked_at = title_map::baked_at_micros();

    let state = Box::new(State {
        rl,
        thread,
        title_map,
        title_map_view,
        title_map_baked_at,
        title_map_requested: false,
        my_identity: None,
        subscription_ready: false,
        tables: Tables::new(),
        camera: Camera2D {
            offset: Vector2::new(360.0, 360.0),
            target: Vector2::new(0.0, 0.0),
            rotation: 0.0,
            zoom: ISLAND_FIT_ZOOM,
        },
        centered_on_island: false,
        intro_started_at: None,
        intro_from: None,
        ui_state: ui::UiState::new(),
        replay_clock: None,
        recovered_replay: None,
        history_pending: None,
        replay_recording: false,
        clean_timeline_export: false,
        gif_recording: false,
        gif_export_island: None,
        gif_frame_counter: 0,
        export_progress_shown: -1,
        export_saved_for: 0.0,
        known_inventory_ids: HashSet::new(),
        inventory_seeded: false,
        known_merge_event_ids: HashSet::new(),
        merge_events_seeded: false,
        known_hexa_event_ids: HashSet::new(),
        hexa_events_seeded: false,
        hexa_display: HashMap::new(),
        last_level: None,
        last_xp: None,
        pending_gift_claim: None,
        music_started: false,
        long_press: None,
        pending_info_click: None,
        hover_target: None,
        pinch: None,
        middle_click: None,
        stroke_last: None,
        last_paint_at: Instant::now(),
        last_sent_pos: None,
        last_sent_at: Instant::now(),
        now_micros: 0,
        suppress_map_until_release: false,
        was_focused: true,
        mouse_state_stale: false,
        sfx,
        audio,
    });
    let arg = Box::into_raw(state) as *mut c_void;
    unsafe {
        emscripten_set_main_loop_arg(on_frame, arg, 0, true);
    }
}
