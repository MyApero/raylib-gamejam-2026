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

#[path = "../world.rs"]
mod world;
#[path = "../ui.rs"]
mod ui;
#[path = "../sfx.rs"]
mod sfx;
use world::constants::*;

use raylib::prelude::*;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::ffi::{c_void, CStr, CString};
use std::os::raw::c_char;
use std::time::{Duration, Instant};

unsafe extern "C" {
    fn emscripten_run_script_string(script: *const c_char) -> *const c_char;
    fn emscripten_set_main_loop_arg(
        func: extern "C" fn(*mut c_void),
        arg: *mut c_void,
        fps: i32,
        simulate_infinite_loop: bool,
    );
}

/// Everything the JS side hands us once per frame — see stdb.frame() in
/// client/web/game.html.
#[derive(Deserialize, Default)]
struct FrameData {
    #[serde(default)]
    identity: Option<String>,
    #[serde(default)]
    now_micros: i64,
    #[serde(default)]
    msgs: Vec<String>,
    /// Author follow-up (2026-07-12): set once by `window.stdb.frame()` the
    /// frame after a trial import connection (see `importToken` in
    /// game.html) gets rejected — read-once, same as `msgs`, so a stale
    /// error can't linger and pop up again on some unrelated later frame.
    #[serde(default)]
    import_error: Option<String>,
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
    /// F13: needed (unlike everywhere else that's skipped `obtained_at` —
    /// see `GiftRow`'s comment) to join a `None`-`obtained_with_hex` row
    /// against a `hexa_event` row stamped with the identical `ctx.timestamp`
    /// — see `apply_hexa`'s server-side comment on why this is a timestamp
    /// join rather than a new bool field.
    obtained_at_micros: i64,
    obtained_with_hex: Option<String>,
    /// F11: mirrors `main.rs`'s `Inventory.from_gift` — a flying-gift hue
    /// grant, distinct from `reset_account`'s reseed even though both leave
    /// `obtained_with_hex: None`.
    from_gift: bool,
}

/// F11: mirrors `main.rs`'s `Gift` binding — one row = the single currently-
/// active flying-gift pickup. `expires_at` isn't read client-side (the row
/// simply disappears once the server's tick expires it), so it's skipped
/// here, same as `parse_inventory` already skips `obtained_at`.
struct GiftRow {
    x: f32,
    y: f32,
    spawned_at_micros: i64,
}

/// F13: mirrors `main.rs`'s `HexaEvent` binding, trimmed to the one field
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

/// F13: mirrors `main.rs`'s `HexaCluster` binding — one row per currently-
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
}

struct MarginCellRow {
    q: i32,
    r: i32,
    color: u32,
}

/// F8: one row per (island, liker) — see the server-side comment on
/// `IslandLike` for why uniqueness is enforced in the reducer, not here.
struct IslandLikeRow {
    island_id: u32,
    liker_hex: String,
}

/// F8: `next_rerank_at` drives the countdown banner. `admin_hex` (author
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
            obtained_with_hex: r.field("obtained_with", 4)?.pipe(opt_value).and_then(identity_hex),
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
            itch_rate_id: r.field("itch_rate_id", 4)?.pipe(opt_value).and_then(|v| v.as_u64()).map(|n| n as u32),
            created_at_micros: timestamp_micros(r.field("created_at", 5)?),
            border_color: r.field("border_color", 6)?.pipe(opt_value).and_then(|v| v.as_u64()).map(|n| n as u32),
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
            next_rerank_at_micros: r.field("next_rerank_at", 3)?.pipe(opt_value).map(timestamp_micros),
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
    Some((id, HexaEventRow { at_micros: timestamp_micros(r.field("at", 1)?) }))
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
                "hexa_cluster" => apply_updates(&mut self.hexa_clusters, updates, parse_hexa_cluster),
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
    run_js(&format!("window.stdb && window.stdb.callReducer({js_name}, {js_args})"));
}


/// What a hovered world cell is paintable as, from `me`'s point of view —
/// mirrors `main.rs`'s `Paintable` against this file's plain `Tables`
/// instead of `ctx.db`.
enum Paintable {
    OwnIsland(i32, i32),
    Margin(i32, i32),
    /// F14 (decision 20): the slot-0 community island — paintable by anyone.
    Community(i32, i32),
    /// Author request (admin "draw anywhere"): absolute world coords on
    /// someone else's island, paintable only because `me` is admin.
    Anywhere(i32, i32),
    None,
}

fn my_island<'a>(tables: &'a Tables, me: &str) -> Option<(&'a IslandRow, u32)> {
    tables.islands.iter().find(|(_, isl)| isl.owner_hex == me).map(|(&id, isl)| (isl, id))
}

/// Author request (admin "draw anywhere"): true once `me` has claimed the
/// admin role via `claim_admin` — mirrors `main.rs`'s `is_admin` against this
/// file's plain `Tables` instead of `ctx.db`.
fn is_admin(tables: &Tables, me: &str) -> bool {
    tables.configs.get(&0).is_some_and(|c| c.admin_hex.as_deref() == Some(me))
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
    // F14: slot 0 is always the origin (fixed geometry, never a real
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

/// F9.6 item 2 (middle-click eyedropper): mirrors `main.rs`'s
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
    tables.margin_cells.values().find(|c| c.q == world_q && c.r == world_r).map(|c| world::unpack_hsv(c.color))
}

/// F9.6 item 7 (launch intro): mirrors `main.rs`'s `world_fit` — a camera
/// pose framing every currently-known island's center.
fn world_fit(tables: &Tables, fallback: Vector2, fallback_zoom: f32) -> (Vector2, f32) {
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
    let zoom = (620.0 / span.max(1.0)).clamp(0.25, ISLAND_FIT_ZOOM);
    (target, zoom)
}

/// Display label for a merge partner in the "new color" toast: their name if
/// set (every player gets a random one at first connect, server-side).
/// Falls back to a generic label, never the partner's identity — mirrors
/// `main.rs`'s `player_label`, see its comment for why.
fn player_label(tables: &Tables, id: &str) -> String {
    // F14 (decision 20): mirrors `main.rs` — the community island's sentinel
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
    sfx: Option<&sfx::Sfx<'_>>,
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
            ui_state.show_info_toast("not unlocked — long-press to merge".to_string());
            if let Some(s) = sfx {
                s.error.play();
            }
            false
        }
        world::EyedropperPick::Empty => {
            ui_state.show_info_toast("no painted color here".to_string());
            false
        }
    }
}

/// F8: mirrors `main.rs`'s `format_age` exactly, just in raw micros instead
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
    tables.island_likes.values().any(|l| l.island_id == island_id && l.liker_hex == me)
}

/// Author-requested: mirrors `main.rs`'s `resolve_border_color` — what an
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

/// F8: mirrors `main.rs`'s `open_island_info`, reading from the local
/// `Tables` cache instead of `ctx.db`.
fn open_island_info(state: &mut State, island_id: u32) {
    let Some(island) = state.tables.islands.get(&island_id) else { return };
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
    state.ui_state.open_island_info(ui::IslandInfo { island_id, owner_label, likes, age_label, link_id, is_own, already_liked, border_hidden, border_color });
}

/// Author follow-up (2026-07-12): mirrors `main.rs`'s `open_admin_edit`,
/// reading from the local `Tables` cache instead of `ctx.db`.
fn open_admin_edit(state: &mut State, island_id: u32) {
    let Some(island) = state.tables.islands.get(&island_id) else { return };
    let Some(owner) = state.tables.users.get(&island.owner_hex) else { return };
    let name = owner.name.clone().unwrap_or_default();
    let likes = island.likes;
    let xp = owner.xp;
    state.ui_state.open_admin_edit(island_id, &name, likes, xp);
}

/// In-flight long-press-to-merge gesture — mirrors `main.rs`'s `LongPress`.
/// Also tracks the F8 island-info target, fired instead on a plain click —
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

struct State {
    rl: RaylibHandle,
    thread: RaylibThread,
    my_identity: Option<String>,
    tables: Tables,
    camera: Camera2D,
    centered_on_island: bool,
    /// F9.6 item 7: mirrors `main.rs`'s `intro_started_at`/`intro_from`.
    intro_started_at: Option<Instant>,
    intro_from: Option<(Vector2, f32)>,
    ui_state: ui::UiState,
    known_inventory_ids: HashSet<u64>,
    inventory_seeded: bool,
    known_merge_event_ids: HashSet<u64>,
    merge_events_seeded: bool,
    known_hexa_event_ids: HashSet<u64>,
    hexa_events_seeded: bool,
    /// F13: mirrors `main.rs`'s `hexa_display` — persisted, lerped hexagon-
    /// vertex snap positions (including the local player's own, per the
    /// author) keyed by identity hex string (this client has no SDK
    /// `Identity` type).
    hexa_display: HashMap<String, Vector2>,
    /// F9 level-up toast: mirrors `main.rs`'s `last_level` — `None` until the
    /// first frame `me` is known, so connecting already at some level
    /// doesn't fire a spurious toast.
    last_level: Option<u64>,
    long_press: Option<LongPress>,
    /// Author-requested: mirrors `main.rs`'s `pending_info_click` — a clean
    /// single-click on a foreign island, held pending for
    /// DOUBLE_CLICK_WINDOW before it resolves into actually opening the
    /// info popup (see the gesture block's comment for why).
    pending_info_click: Option<(Instant, Vector2, u32)>,
    /// F9.5 item 7 / decision 17: mirrors `main.rs`'s `hover_target` —
    /// `(island_id, hover_started_at)` for the currently-hovered foreign
    /// island, tracked purely by cursor position, independent of
    /// `pending_info_click` above (left unchanged for double-click-to-like
    /// and touch).
    hover_target: Option<(u32, Instant)>,
    pinch: Option<Pinch>,
    /// F9.6 item 2: mirrors `main.rs`'s `middle_click`.
    middle_click: Option<Vector2>,
    stroke_last: Option<(u8, i32, i32)>,
    last_paint_at: Instant,
    last_sent_pos: Option<Vector2>,
    last_sent_at: Instant,
    now_micros: i64,
    /// F9.5 item 5 (modal click-through): mirrors `main.rs`'s
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
    /// F12: `None` if the audio device failed to init (no sound card,
    /// browser autoplay block, headless) — every call site degrades to
    /// silence instead of unwrapping. Backed by a leaked `'static`
    /// `RaylibAudio` (see `main`'s comment) — this `State` itself is never
    /// freed either (`Box::into_raw`, below), so leaking the audio device
    /// alongside it for the process's whole lifetime is the same tradeoff,
    /// not a new one.
    sfx: Option<sfx::Sfx<'static>>,
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
        // Reconnects replay the full table; drop stale local state first.
        state.tables.clear();
        if let Some(db_update) = initial.get("database_update") {
            state.tables.apply(db_update);
        }
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

fn recenter(camera: &mut Camera2D, island: &IslandRow) {
    camera.target = island_world_center(island);
    camera.zoom = ISLAND_FIT_ZOOM;
    // Mouse-wheel/pinch zoom re-anchors `offset` to the cursor/fingers, so
    // reset it back to screen-center or the island lands off-target.
    camera.offset = Vector2::new(360.0, 360.0);
}

extern "C" fn on_frame(arg: *mut c_void) {
    let state = unsafe { &mut *(arg as *mut State) };
    frame(state);
}

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
        && !state.rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT)
        && !state.rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_MIDDLE)
        && !state.rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_RIGHT)
    {
        state.mouse_state_stale = false;
    }
    // One JS call pulls in everything the socket received since last frame.
    let raw = run_js("window.stdb ? window.stdb.frame() : '{}'");
    let data: FrameData = serde_json::from_str(&raw).unwrap_or_default();
    state.now_micros = data.now_micros;
    if state.my_identity.is_none() {
        state.my_identity = data.identity.as_deref().map(normalize_identity);
    }
    for msg in &data.msgs {
        handle_message(state, msg);
    }
    // Author follow-up (2026-07-12): a rejected Import — the current account
    // and connection are untouched (see `importToken` in game.html), so this
    // is just user feedback, not a reconnect.
    if let Some(err) = data.import_error {
        state.ui_state.show_info_toast(err);
    }

    let me = state.my_identity.clone();
    let me = me.as_deref();
    let hexa_zoom_locked = me.is_some_and(|id| state.tables.hexa_clusters.contains_key(id));
    if hexa_zoom_locked {
        let rate = (1.0 - (-state.rl.get_frame_time() / world::constants::HEXA_ZOOM_LERP_SECS).exp()).clamp(0.0, 1.0);
        state.camera.zoom += (ISLAND_FIT_ZOOM - state.camera.zoom) * rate;
        if (state.camera.zoom - ISLAND_FIT_ZOOM).abs() < 0.001 {
            state.camera.zoom = ISLAND_FIT_ZOOM;
        }
    }
    let mouse_screen = state.rl.get_mouse_position();
    let mouse_world = state.rl.get_screen_to_world2D(mouse_screen, state.camera);

    // F11 (flying gift): mirrors `main.rs` exactly — current drifted world
    // position of the single active gift (if any) and whether the mouse is
    // within claim range of it right now.
    let active_gift: Option<(u64, Vector2, f32)> = state.tables.gifts.iter().next().map(|(&id, g)| {
        let elapsed = ((state.now_micros - g.spawned_at_micros).max(0) as f64 / 1_000_000.0) as f32;
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
    let over_map_area =
        !state.ui_state.title_active && mouse_screen.y > ui::HEADER_H && mouse_screen.y < (720.0 - ui::FOOTER_H);

    // F9.6 item 7: launch intro — mirrors `main.rs` exactly. Camera starts
    // framing the whole occupied world and eases to the player's island over
    // `INTRO_DURATION`; any input skips straight to the final pose.
    //
    // Title screen (author-requested): mirrors `main.rs` — while it's up,
    // hold the camera on the whole-occupied-world pose so the
    // semi-transparent backdrop shows a hint of the full map; the intro
    // only starts once the Draw button dismisses the title, easing from
    // this exact pose (`intro_from` calls the same `world_fit`).
    if state.ui_state.title_active {
        if let Some(me) = me {
            if let Some((island, _)) = my_island(&state.tables, me) {
                let (target, zoom) = world_fit(&state.tables, island_world_center(island), ISLAND_FIT_ZOOM);
                state.camera.target = target;
                state.camera.zoom = zoom;
                state.camera.offset = Vector2::new(360.0, 360.0);
            }
        }
    } else if let Some(me) = me {
        if !state.centered_on_island {
            if let Some((island, _)) = my_island(&state.tables, me) {
                let to_target = island_world_center(island);
                let to_zoom = ISLAND_FIT_ZOOM;
                let start = *state.intro_started_at.get_or_insert_with(Instant::now);
                let (from_target, from_zoom) =
                    *state.intro_from.get_or_insert_with(|| world_fit(&state.tables, to_target, to_zoom));
                let any_input = state.rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT)
                    || state.rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_MIDDLE)
                    || state.rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_RIGHT)
                    || state.rl.get_mouse_wheel_move() != 0.0
                    || state.rl.get_touch_point_count() > 0
                    || state.rl.get_key_pressed().is_some();
                let t = (start.elapsed().as_secs_f32() / INTRO_DURATION.as_secs_f32()).clamp(0.0, 1.0);
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

    // Inventory-insert watch (merge toast): toast + last3 nudge when a new
    // row for `me` appears. Mirrors `main.rs` exactly.
    if let Some(me) = me {
        if !state.merge_events_seeded {
            state.known_merge_event_ids = state.tables.merge_events.keys().copied().collect();
            state.merge_events_seeded = true;
        } else {
            for (&id, event) in &state.tables.merge_events {
                if state.known_merge_event_ids.insert(id) && (event.a_hex == me || event.b_hex == me) {
                    state.ui_state.note_used_color(ui::RecentColor { hue: event.merged_hue, sat: event.merged_sat, val: event.merged_val });
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
                    && state.tables.hexa_clusters.get(me).is_some_and(|row| row.ignited)
                {
                    state.ui_state.show_hexa_success_popup();
                    if let Some(s) = &state.sfx {
                        s.merge.play();
                    }
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
                    // F9.5 item 4: mirrors `main.rs` — a NEW obtained_with-
                    // less row after the initial seed can only be
                    // `reset_account`'s reseed, never a merge.
                    if inv.from_gift {
                        state.ui_state.show_gift_toast(inv.hue);
                        if let Some(s) = &state.sfx {
                            s.gift.play();
                        }
                    } else if inv.obtained_with_hex.is_none() {
                        // F13: mirrors `main.rs` — a Hexa-pooled grant also
                        // leaves `obtained_with_hex: None`; told apart from a
                        // `reset_account` reseed by joining against a
                        // `hexa_event` row stamped with the identical
                        // `ctx.timestamp` (see `apply_hexa`'s server-side
                        // comment).
                        if state.tables.hexa_events.values().any(|e| e.at_micros == inv.obtained_at_micros) {
                            state.ui_state.show_hexa_toast(inv.hue);
                            if let Some(s) = &state.sfx {
                                s.merge.play();
                            }
                        } else {
                            state.ui_state.note_reset_hue(inv.hue);
                        }
                    } else {
                        let label = inv
                            .obtained_with_hex
                            .as_deref()
                            .map_or_else(|| "someone".to_string(), |p| player_label(&state.tables, p));
                        // Mirrors `main.rs`: use the same
                        // `obtained_at`/`MergeEvent.at` timestamp join
                        // the Hexa event branch above already uses.
                        let merge_from = state
                            .tables
                            .merge_events
                            .values()
                            .find(|e| e.at_micros == inv.obtained_at_micros && (e.a_hex == me || e.b_hex == me))
                            .map(|e| if e.a_hex == me { (e.hue_a, e.hue_b) } else { (e.hue_b, e.hue_a) });
                        let color = state
                            .tables
                            .merge_events
                            .values()
                            .find(|e| e.at_micros == inv.obtained_at_micros && (e.a_hex == me || e.b_hex == me))
                            .map(|e| ui::RecentColor { hue: e.merged_hue, sat: e.merged_sat, val: e.merged_val })
                            .or_else(|| state.tables.users.get(me).map(|u| ui::RecentColor { hue: inv.hue, sat: u.sat, val: u.val }))
                            .unwrap_or(ui::RecentColor { hue: inv.hue, sat: world::sat_cap(0), val: 90 });
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
        // F9.5 item 4: mirrors `main.rs` — seed the last-3 ring with the
        // caller's current hue the first frame it's known.
        if let Some(user) = state.tables.users.get(me) {
            state.ui_state.seed_recent_once(ui::RecentColor { hue: user.hue, sat: user.sat, val: user.val });
        }
    }

    let online = state.tables.users.values().filter(|u| u.online).count();
    let total = state.tables.users.len();

    // F9.5 item 5 (modal click-through): mirrors `main.rs` — latch at
    // press-start whether a modal was open, held for the whole press (a
    // click's press and release land on different frames), so the click
    // that closes an overlay can't also paint the cell behind it on a later
    // frame where the button is still held but the overlay's already gone.
    if state.rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT) {
        // F9.5 item 7 follow-up: mirrors `main.rs` — `any_modal_open`
        // deliberately excludes the foreign-island hover tooltip, which has
        // no interactive chrome and must not block map input.
        state.suppress_map_until_release = state.ui_state.any_modal_open();
        // F11: mirrors `main.rs` — a press landing on the gift claims it
        // immediately and consumes the whole gesture so the same press can't
        // also start a paint stroke or long-press underneath it.
        if !state.suppress_map_until_release && over_map_area && gift_hit {
            if let Some((gift_id, _, _)) = active_gift {
                call_reducer("claim_gift", serde_json::json!([gift_id]));
            }
            state.suppress_map_until_release = true;
        }
    }
    if state.rl.is_mouse_button_released(MouseButton::MOUSE_BUTTON_LEFT) {
        state.suppress_map_until_release = false;
    }

    // HUD: snapshot server state, run widget input, apply resulting reducer
    // calls. Must run before the map-input blocks below so they can see
    // `ui_state.any_modal_open()`.
    // `Some(name)` marks this frame as the clean, camera-centred export
    // card. It is intentionally ephemeral: after the canvas is captured at
    // the end of this frame, the normal game returns immediately.
    let mut export_name: Option<String> = None;
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
        // F9 level-up feedback: mirrors `main.rs` exactly.
        if state.last_level.is_some_and(|prev| prev < shared::constants::HEXA_UNLOCK_LEVEL && level >= shared::constants::HEXA_UNLOCK_LEVEL) {
            state.ui_state.show_hexa_unlocked_toast();
            if let Some(s) = &state.sfx {
                s.levelup.play();
            }
        } else if state.last_level.is_some_and(|prev| level > prev) {
            state.ui_state.show_levelup_toast(level, world::sat_cap(level), hue);
            if let Some(s) = &state.sfx {
                s.levelup.play();
            }
        }
        state.last_level = Some(level);
        state.ui_state.sync_name_once(user.and_then(|u| u.name.as_ref()));
        // Author-caught: mirrors `main.rs`'s live-refresh so the Like button
        // reflects the reducer's result immediately, not only after a page
        // reload (the popup used to be a one-time snapshot from open time).
        if let Some(island_id) = state.ui_state.island_popup.as_ref().map(|p| p.island_id) {
            if let Some(island) = state.tables.islands.get(&island_id) {
                let likes = island.likes;
                let liked = already_liked(&state.tables, island_id, me);
                let border_color = resolve_border_color(&state.tables, island);
                state.ui_state.refresh_island_popup(likes, liked, island.border_hidden, border_color);
            }
        }

        let short_id = me.to_string();
        // F8 re-rank countdown: only `Some` while the target is still ahead
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
        let actions = ui::handle_input(&mut state.rl, &mut state.ui_state, &info);
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
            run_js(&format!("window.stdb && window.stdb.importToken({js_token})"));
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
            call_reducer("admin_update_island", serde_json::json!([island_id, name, likes, xp]));
        }
        if let Some(island_id) = actions.admin_delete_island {
            call_reducer("admin_delete_island", serde_json::json!([island_id]));
        }
        if actions.admin_force_rerank {
            call_reducer("admin_force_rerank", serde_json::json!([]));
        }
        if let Some((island_id, rate_id)) = actions.click_link {
            // plan.md F9: web opens the rate page via `window.open`, unlike
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
        // Author-requested: footer button replacing the old "click your own
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
        if actions.export_screenshot {
            if let Some((island, _)) = my_island(&state.tables, me) {
                // A share image always shows the caller's island, not their
                // potentially zoomed-out/panned current map view.
                recenter(&mut state.camera, island);
                export_name = Some(state.ui_state.name_input.clone());
            }
        }
    }
    // F9.5 item 7 follow-up: mirrors `main.rs`.
    let map_input_allowed = !state.suppress_map_until_release && !state.ui_state.any_modal_open();

    // Two-finger pinch/pan (touch); single-finger tap/drag is already
    // translated to ordinary mouse events by raylib's web backend, so the
    // mouse-driven paint/pan/long-press code below covers it unchanged.
    let touch_count = state.rl.get_touch_point_count();
    let gesturing = touch_count >= 2;
    if map_input_allowed && gesturing {
        let t0 = state.rl.get_touch_position(0);
        let t1 = state.rl.get_touch_position(1);
        let mid = Vector2::new((t0.x + t1.x) / 2.0, (t0.y + t1.y) / 2.0);
        let dist = ((t1.x - t0.x).powi(2) + (t1.y - t0.y).powi(2)).sqrt().max(1.0);
        if state.pinch.is_none() {
            let anchor_world = state.rl.get_screen_to_world2D(mid, state.camera);
            state.pinch = Some(Pinch { anchor_world, start_dist: dist, start_zoom: state.camera.zoom });
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
    let wheel = if map_input_allowed && !gesturing && !hexa_zoom_locked { state.rl.get_mouse_wheel_move() } else { 0.0 };
    if wheel != 0.0 {
        state.camera.offset = mouse_screen;
        state.camera.target = mouse_world;
        state.camera.zoom = (state.camera.zoom * (1.0 + wheel * 0.1)).clamp(0.25, 60.0);
    }

    // F9.6 item 6: WASD/arrow-key pan, Q/E zoom — mirrors `main.rs` exactly,
    // including the text-field-focus guard.
    if map_input_allowed && !gesturing && !state.ui_state.text_field_focused() {
        let dt = state.rl.get_frame_time();
        let mut dx = 0.0;
        let mut dy = 0.0;
        if state.rl.is_key_down(KeyboardKey::KEY_LEFT) || state.rl.is_key_down(KeyboardKey::KEY_A) {
            dx -= 1.0;
        }
        if state.rl.is_key_down(KeyboardKey::KEY_RIGHT) || state.rl.is_key_down(KeyboardKey::KEY_D) {
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

    // F13 follow-up: the Move tool makes plain left-drag pan too, no Shift
    // needed — mirrors `main.rs`.
    let panning = map_input_allowed
        && !gesturing
        && !state.mouse_state_stale
        && (state.rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_MIDDLE)
            || state.rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_RIGHT)
            || (state.rl.is_key_down(KeyboardKey::KEY_LEFT_SHIFT) && state.rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT))
            || (state.ui_state.tool == ui::Tool::Move && state.rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT)));
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
        let moved = state
            .last_sent_pos
            .is_none_or(|p| (p.x - mouse_world.x).abs() > 1e-4 || (p.y - mouse_world.y).abs() > 1e-4);
        if moved && state.last_sent_at.elapsed() >= Duration::from_secs_f32(1.0 / CURSOR_SEND_HZ) {
            call_reducer("set_pos", serde_json::json!([mouse_world.x, mouse_world.y]));
            state.last_sent_pos = Some(mouse_world);
            state.last_sent_at = Instant::now();
        }
    }

    // Painting/erasing: left-drag (or one-finger touch-drag), not while
    // panning (SHIFT/middle/right, two-finger gesturing, or the Move tool —
    // see `panning` above) or two-finger gesturing. F9.6 item 1: which
    // reducer fires depends on `ui_state.tool` — mirrors `main.rs` exactly,
    // including the `over_map_area` guard against the header/footer HUD.
    if map_input_allowed
        && !gesturing
        && over_map_area
        && matches!(state.ui_state.tool, ui::Tool::Paint | ui::Tool::Erase)
    {
        if let Some(me) = me {
            if state.rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT) && !panning && !state.mouse_state_stale {
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
                    let rate_ok = state.last_paint_at.elapsed() >= Duration::from_secs_f32(1.0 / CLIENT_PAINT_HZ);
                    if fresh_cell && rate_ok {
                        match (key, state.ui_state.tool == ui::Tool::Erase) {
                            ((0, lq, lr), false) => call_reducer("paint_island_cell", serde_json::json!([lq, lr])),
                            ((0, lq, lr), true) => call_reducer("erase_island_cell", serde_json::json!([lq, lr])),
                            ((1, q, r), false) => call_reducer("paint_margin_cell", serde_json::json!([q, r])),
                            ((1, q, r), true) => call_reducer("erase_margin_cell", serde_json::json!([q, r])),
                            ((2, lq, lr), false) => call_reducer("paint_community_cell", serde_json::json!([lq, lr])),
                            ((2, lq, lr), true) => call_reducer("erase_community_cell", serde_json::json!([lq, lr])),
                            ((3, q, r), false) => call_reducer("admin_paint_cell", serde_json::json!([q, r])),
                            ((3, q, r), true) => call_reducer("admin_erase_cell", serde_json::json!([q, r])),
                            _ => unreachable!(),
                        }
                        state.stroke_last = Some(key);
                        state.last_paint_at = Instant::now();
                    }
                }
            }
        }
    }
    if state.rl.is_mouse_button_released(MouseButton::MOUSE_BUTTON_LEFT) {
        state.stroke_last = None;
    }

    // One-shot click/tap eyedropper for mobile and trackpads. Middle-click
    // remains available as the desktop shortcut below.
    if map_input_allowed
        && !gesturing
        && over_map_area
        && state.ui_state.tool == ui::Tool::Eyedropper
        && state.rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT)
    {
        if let Some(me) = me {
            let (wq, wr) = world::world_to_axial(mouse_world);
            if pick_color_at(&state.tables, &mut state.ui_state, state.sfx.as_ref(), me, wq, wr) {
                if state.ui_state.finish_eyedropper() {
                    call_reducer("set_lock", serde_json::json!([false]));
                }
            }
        }
    }

    // Author follow-up (2026-07-12): mirrors `main.rs`'s AdminEdit click
    // block — a persistent, mobile-friendly tool state (not a right-click/
    // long-press) whose only effect is opening the admin edit modal on tap.
    if map_input_allowed
        && !gesturing
        && over_map_area
        && state.ui_state.tool == ui::Tool::AdminEdit
        && state.rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT)
    {
        if let Some(me) = me {
            let (wq, wr) = world::world_to_axial(mouse_world);
            if let Some((island_id, _, _)) = island_at(&state.tables, wq, wr) {
                let editable = state
                    .tables
                    .islands
                    .get(&island_id)
                    .is_some_and(|isl| isl.owner_hex != me && isl.owner_hex != world::COMMUNITY_OWNER_HEX);
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
        if state.rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_MIDDLE) && over_map_area {
            state.middle_click = Some(mouse_screen);
        }
        if state.rl.is_mouse_button_released(MouseButton::MOUSE_BUTTON_MIDDLE) {
            if let Some(press) = state.middle_click.take() {
                let dx = mouse_screen.x - press.x;
                let dy = mouse_screen.y - press.y;
                if (dx * dx + dy * dy).sqrt() <= MIDDLE_CLICK_TOL_PX {
                    if let Some(me) = me {
                        let (wq, wr) = world::world_to_axial(mouse_world);
                        if pick_color_at(&state.tables, &mut state.ui_state, state.sfx.as_ref(), me, wq, wr) {
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
        // Author follow-up: `AdminEdit` is handled entirely by its own
        // block above — mirrors `main.rs`'s exclusion here.
        if state.ui_state.tool != ui::Tool::Eyedropper
            && state.ui_state.tool != ui::Tool::AdminEdit
            && !panning
            && over_map_area
            && state.rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT)
        {
            let (wq, wr) = world::world_to_axial(mouse_world);
            state.long_press = Some(LongPress {
                press_screen: mouse_screen,
                press_at: Instant::now(),
                target: merge_target_at(&state.tables, wq, wr).filter(|&(_, _, hue)| !have_hue(&state.tables, me, hue)),
                // F8 (author follow-up): anywhere on a FOREIGN island's
                // territory, not just its center. Mirrors `main.rs`; own
                // island's popup now opens via the "My Isle" footer button.
                // F14 author reversal: the community island is excluded here
                // — no info popup, no like, matching the hover exclusion
                // below. Its sentinel owner would otherwise pass this
                // `owner_hex != me` check like any other foreign island.
                info_target: island_at(&state.tables, wq, wr)
                    .filter(|&(id, _, _)| {
                        state.tables.islands.get(&id).is_some_and(|isl| isl.owner_hex != me && isl.owner_hex != world::COMMUNITY_OWNER_HEX)
                    })
                    .map(|(id, _, _)| id),
                fired: false,
            });
        }
        if let Some(lp) = &mut state.long_press {
            let dx = mouse_screen.x - lp.press_screen.x;
            let dy = mouse_screen.y - lp.press_screen.y;
            let moved = (dx * dx + dy * dy).sqrt() > LONG_PRESS_TOL_PX;
            let released = !state.rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT);
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
                            state.pending_info_click = Some((Instant::now(), mouse_screen, island_id));
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
            // F9.5 item 7 follow-up (author-requested): mirrors `main.rs` —
            // the hover tooltip that replaced this click's old
            // popup-opening role is non-interactive, so a resolved single
            // click on a foreign island now directly opens its itch.io link
            // (if set) too — also decision 17's "tap opens" path for touch,
            // which has no hover.
            if let Some(rate_id) = state.tables.islands.get(&island_id).and_then(|isl| isl.itch_rate_id) {
                let url = format!("https://itch.io/jam/raylib-6x-gamejam/rate/{rate_id}");
                let js_url = serde_json::to_string(&url).unwrap();
                run_js(&format!("window.open({js_url}, '_blank')"));
                call_reducer("click_link", serde_json::json!([island_id]));
            }
            state.pending_info_click = None;
        }
    }

    // F9.5 item 7 / decision 17: island info on hover (desktop). Mirrors
    // `main.rs` — purely position-based, independent of the click/long-press
    // gesture block above (left untouched for double-click-to-like and
    // touch; raylib-web aliases a single touch to ordinary mouse events, so
    // that path already covers touch taps). Author-caught: excludes the
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
                        state.tables.islands.get(&id).is_some_and(|isl| isl.owner_hex != me && isl.owner_hex != world::COMMUNITY_OWNER_HEX)
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
            let gesturing_input = state.rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT)
                || state.rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_MIDDLE)
                || gesturing;
            // `any_modal_open` covers My Isle and the Escape/help overlay
            // too — without it, hovering a foreign island would, after the
            // delay, silently overwrite/close whichever modal was open
            // (mirrors main.rs).
            if !state.ui_state.any_modal_open() && !gesturing_input {
                if let Some((hid, since)) = state.hover_target {
                    let already_open = state.ui_state.island_popup.as_ref().is_some_and(|p| p.island_id == id);
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
    let pad = (ISLAND_RADIUS as f32) * 2.0 * 5.0;
    let top_left = state.rl.get_screen_to_world2D(Vector2::new(0.0, 0.0), state.camera);
    let bottom_right = state.rl.get_screen_to_world2D(Vector2::new(720.0, 720.0), state.camera);
    let (view_min_x, view_max_x) = (top_left.x - pad, bottom_right.x + pad);
    let (view_min_y, view_max_y) = (top_left.y - pad, bottom_right.y + pad);
    let in_view = |p: Vector2| p.x >= view_min_x && p.x <= view_max_x && p.y >= view_min_y && p.y <= view_max_y;

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

    // F9.5 item 6: mirrors `main.rs` — shrinks other players' cursors with
    // camera zoom (relative to their fixed size at the default
    // `ISLAND_FIT_ZOOM`), floored so they stay findable when zoomed out.
    let other_cursor_scale = (state.camera.zoom / ISLAND_FIT_ZOOM).max(world::constants::CURSOR_MIN_SCALE);

    // F13 (Hexa event): mirrors `main.rs` — the server-authoritative central
    // formation, with each member placed at ITS OWN
    // `vertex_index` (paired per-row, not by list position). `me`'s own row
    // is included like everyone else's — per the author, the LOCAL player's
    // own cursor should visibly move to its hexagon slot too (see the
    // local-cursor draw call below).
    let hexa_rows: Vec<(&String, &HexaClusterRow)> = state.tables.hexa_clusters.iter().collect();
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
        let members: Vec<(String, u32)> = hexa_rows.iter().map(|&(id, row)| (id.clone(), row.vertex_index)).collect();
        let centre = Vector2::zero();
        let (frame, vertices) = world::hexa_cluster_frame(
            centre,
            first.member_count as usize,
            &members,
            |key, target| {
                if Some(key.as_str()) == me {
                    mouse_world
                } else {
                    state.tables.users.get(key).map(|u| Vector2::new(u.cx, u.cy)).unwrap_or(target)
                }
            },
        );
        for (key, vertex_index) in &members {
            let i = *vertex_index as usize % 6;
            let a = vertices[i];
            let b = vertices[(i + 1) % 6];
            hexa_centres.insert(key.clone(), Vector2::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5));
        }
        cluster_members.extend(frame);
        hexa_polygons.push((vertices, first.ignited));
    }
    state.hexa_display = world::hexa_advance_display(&state.hexa_display, &cluster_members, state.rl.get_frame_time());

    let other_cursors: Vec<(Vector2, Color, bool, String, Option<Vector2>)> = state
        .tables
        .users
        .iter()
        // Author-reported: mirrors `main.rs` — cursors used to vanish ~3s
        // after a player stopped moving; a stationary-but-connected player
        // should stay visible the whole time they're online.
        .filter(|(id, u)| u.online && Some(id.as_str()) != me)
        .map(|(id, u)| {
            // F13: mirrors `main.rs` — a hexagon-cluster member renders at
            // its lerped snapped display position instead of its raw
            // cursor position, and carries its cluster's centre
            // (screen-projected) for the rotated arrow draw.
            let world_pos = state.hexa_display.get(id).copied().unwrap_or(Vector2::new(u.cx, u.cy));
            (
                state.rl.get_world_to_screen2D(world_pos, state.camera),
                world::hsv_color(u.hue, u.sat, u.val),
                u.locked,
                u.name.clone().unwrap_or_default(),
                hexa_centres.get(id).map(|&c| state.rl.get_world_to_screen2D(c, state.camera)),
            )
        })
        .collect();

    // F13 (author follow-up): mirrors `main.rs`'s `my_hexa_screen` — the
    // LOCAL player's own cursor also renders at its lerped hexagon-snap
    // position while participating. `None` (not clustered, or `me` unknown
    // yet) falls back to the literal mouse position at the draw call.
    // Carries (tip, cluster centre), both screen-space, for the rotated
    // snapped-arrow draw.
    let my_hexa_screen: Option<(Vector2, Vector2)> = me.and_then(|me| {
        let pos = state.hexa_display.get(me)?;
        let centre = hexa_centres.get(me)?;
        Some((state.rl.get_world_to_screen2D(*pos, state.camera), state.rl.get_world_to_screen2D(*centre, state.camera)))
    });
    let eyedropper_preview = (state.ui_state.tool == ui::Tool::Eyedropper)
        .then(|| {
            let (q, r) = world::world_to_axial(mouse_world);
            painted_color_at(&state.tables, q, r).map(|(h, s, v)| world::hsv_color(h, s, v))
        })
        .flatten()
        .unwrap_or(Color::new(150, 150, 156, 255));

    let camera = state.camera;
    // F9.6 item 8: mirrors `main.rs` — skip the per-tile outline pass past
    // this zoom (visual noise at that size; a small render win too).
    let show_tile_outline = camera.zoom >= BORDERLESS_ZOOM_THRESHOLD;
    // Grabbed before `begin_drawing` hands out its mutable borrow — mirrors
    // `main.rs`.
    let fps = state.rl.get_fps();
    let hover_takeable;
    let mut d = state.rl.begin_drawing(&state.thread);
    d.clear_background(Color::new(18, 18, 24, 255));

    {
        let mut d2 = d.begin_mode2D(camera);

        for (&island_id, island) in &state.tables.islands {
            let (q, r) = world::slot_coords(island.slot);
            let (fcx, fcy) = world::slot_center(q, r);
            let center = world::axial_to_world(fcx, fcy);
            if !in_view(center) {
                continue;
            }
            let mine = me == Some(island.owner_hex.as_str());
            let unpainted_fill = world::unpainted_island_fill(island.owner_hex == world::COMMUNITY_OWNER_HEX);
            // Mirrors `main.rs`: below `OVERVIEW_ZOOM_THRESHOLD` the
            // island's cells are sub-pixel anyway — draw one flat hex for
            // the whole island instead of 721 individual (and invisible)
            // ones.
            if camera.zoom < world::constants::OVERVIEW_ZOOM_THRESHOLD {
                world::draw_hex(&mut d2, center, ISLAND_RADIUS as f32, unpainted_fill, None);
            } else {
            // F9.5 (FPS at scale): point-lookup each rendered cell by its
            // packed id in the already-id-keyed `island_cells` map instead of
            // collecting a fresh (island_id, q, r) -> color HashMap from
            // EVERY island_cell row in the world every frame — cost is now
            // proportional to in-view cells, not total painted cells.
            for &(dq, dr) in world::island_offsets() {
                let cell_world = world::axial_to_world(fcx + dq, fcy + dr);
                let id = world::island_cell_id(island_id, dq, dr);
                let fill = state.tables.island_cells.get(&id).map_or(unpainted_fill, |c| {
                    let (h, s, v) = world::unpack_hsv(c.color);
                    world::hsv_color(h, s, v)
                });
                world::draw_hex(&mut d2, cell_world, 1.0, fill, show_tile_outline.then_some(Color::new(40, 40, 46, 255)));
            }
            }
            // Author-caught: mirrors `main.rs` — sat/val is now
            // `START_SAT`/`START_VAL` exactly (was a fixed 85/95 lookalike
            // shade).
            //
            // Author-requested: mirrors `main.rs` — an owner-set
            // `border_color`/`border_hidden` override takes priority over
            // the seed-hue default.
            let border_color = if island.border_hidden {
                None
            } else if let Some(packed) = island.border_color {
                let (h, s, v) = world::unpack_hsv(packed);
                Some(world::hsv_color(h, s, v))
            } else {
                seed_hues.get(island.owner_hex.as_str()).map(|&hue| world::hsv_color(hue, START_SAT, START_VAL))
            };
            if let Some(border_color) = border_color {
                let r_f = ISLAND_RADIUS as f32;
                let corners: Vec<Vector2> = world::DIRECTIONS
                    .iter()
                    .map(|&(dq, dr)| world::axial_to_world(fcx + (dq as f32 * r_f) as i32, fcy + (dr as f32 * r_f) as i32))
                    .collect();
                // Own island's border is thicker — still colored by
                // identity like every other island, just easier to spot.
                // Author-caught: mirrors `main.rs` — thickness is now a
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

        for cell in state.tables.margin_cells.values() {
            let p = world::axial_to_world(cell.q, cell.r);
            if !in_view(p) {
                continue;
            }
            let (h, s, v) = world::unpack_hsv(cell.color);
            world::draw_hex(&mut d2, p, 1.0, world::hsv_color(h, s, v), show_tile_outline.then_some(Color::new(30, 30, 34, 255)));
        }

        // F11 (flying gift): world-space, mirrors `main.rs`.
        if let Some((_, pos, elapsed)) = active_gift {
            world::draw_gift_icon(&mut d2, pos, elapsed);
        }

        // F13 (Hexa event): world-space hexagon edges, mirrors `main.rs`.
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
            d2.draw_poly_lines_ex(hover_center, 6, 1.0, 0.0, HOVER_BORDER_PX / camera.zoom, Color::new(255, 255, 255, 210));
        }

        hover_takeable = map_input_allowed
            && over_map_area
            && me.is_some_and(|me| {
                matches!(classify(&state.tables, me, hq, hr), Paintable::None)
                    && merge_target_at(&state.tables, hq, hr).is_some_and(|(_, _, hue)| !have_hue(&state.tables, me, hue))
            });
    }

    let hexa_cursor_scale = camera.zoom / ISLAND_FIT_ZOOM;
    if export_name.is_none() {
        for (screen, color, locked, name, snap_centre) in &other_cursors {
            match snap_centre {
                // F13 follow-up #2: mirrors `main.rs` — a hexagon-snapped
                // cursor aims its tip at the cluster centre.
                Some(centre) => world::draw_cursor_snapped(&mut d, *screen, *centre, *color, hexa_cursor_scale, *locked),
                None => world::draw_cursor_scaled(&mut d, *screen, *color, other_cursor_scale, *locked),
            }
            if snap_centre.is_none() && other_cursor_scale >= 0.5 {
                world::draw_cursor_label(&mut d, *screen, name, other_cursor_scale);
            }
        }
    }

    let own_brush = me.and_then(|me| state.tables.users.get(me)).map(|u| ((u.hue, u.sat, u.val), u.xp, u.locked));
    if let (Some(me), Some((brush, xp, locked))) = (me, own_brush) {
        let hues: Vec<u16> = state.tables.inventory.values().filter(|i| i.owner_hex == me).map(|i| i.hue).collect();
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
        if let Some(name) = export_name.as_deref() {
            ui::draw_export_frame(&mut d, name, info.link_id);
        } else {
            ui::draw(&mut d, &state.ui_state, &info, mouse_screen);

            // F9.6 item 1: mirrors `main.rs` — eraser mode draws the cursor in a
            // neutral gray plus a small eraser badge instead of the brush hue.
            let (hue, sat, val) = brush;
            let cursor_color = if state.ui_state.tool == ui::Tool::Erase {
                Color::new(210, 210, 216, 255)
            } else {
                world::hsv_color(hue, sat, val)
            };
            // F13 (author follow-up): mirrors `main.rs` — visually snaps to the
            // hexagon slot while merging; painting/hover logic still uses the
            // real `mouse_world`/`mouse_screen`, only this draw call moves
            // (and, follow-up #2, rotates to aim at the cluster centre).
            if state.ui_state.tool == ui::Tool::Eyedropper {
                ui::draw_eyedropper_cursor(&mut d, mouse_screen, eyedropper_preview);
            } else {
                match my_hexa_screen {
                    Some((tip, centre)) => world::draw_cursor_snapped(&mut d, tip, centre, cursor_color, hexa_cursor_scale, locked),
                    None => world::draw_cursor(&mut d, mouse_screen, cursor_color, locked),
                }
            }
        }
    }
    if export_name.is_none() {
        if state.ui_state.tool == ui::Tool::Erase {
            world::draw_eraser_badge(&mut d, mouse_screen);
        } else if hover_takeable && matches!(state.ui_state.tool, ui::Tool::Paint | ui::Tool::Erase) {
            // Move tool: left-drag pans instead of merging, so the "+"
            // take-hint (which promises a long-press merge) would mislead.
            world::draw_plus_hint(&mut d, mouse_screen);
        }
        if let Some(frac) = state
            .long_press
            .as_ref()
            .filter(|lp| !lp.fired && lp.target.is_some())
            .map(|lp| (lp.press_at.elapsed().as_secs_f32() / LONG_PRESS_HOLD.as_secs_f32()).clamp(0.0, 1.0))
        {
            world::draw_hold_ring(&mut d, mouse_screen, frac);
        }
        // Hidden on the title screen, colored to match the header's grey —
        // mirrors `main.rs`.
        if !state.ui_state.title_active {
            world::draw_fps_grey(&mut d, 540, 8, fps);
        }
    }
    // EndDrawing must complete before the browser reads the finished canvas.
    drop(d);
    if export_name.is_some() {
        run_js("window.stdb && window.stdb.downloadIslandImage()");
        state.ui_state.show_info_toast("island image downloaded".to_string());
    }
}

fn main() {
    let (mut rl, thread) = raylib::init()
        .size(720, 720) // jam hard constraint
        .title("hexel — raylib 6.0 + SpacetimeDB (web)")
        .build();
    rl.hide_cursor(); // we draw our own pointer in the caller's brush color

    // F12: leaked alongside `state` below (both live for the process's
    // whole lifetime under `emscripten_set_main_loop_arg` — see its comment).
    let audio: Option<&'static RaylibAudio> = RaylibAudio::init_audio_device().ok().map(|a| &*Box::leak(Box::new(a)));
    let sfx = audio.map(sfx::Sfx::load);

    let state = Box::new(State {
        rl,
        thread,
        my_identity: None,
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
        known_inventory_ids: HashSet::new(),
        inventory_seeded: false,
        known_merge_event_ids: HashSet::new(),
        merge_events_seeded: false,
        known_hexa_event_ids: HashSet::new(),
        hexa_events_seeded: false,
        hexa_display: HashMap::new(),
        last_level: None,
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
    });
    let arg = Box::into_raw(state) as *mut c_void;
    unsafe {
        emscripten_set_main_loop_arg(on_frame, arg, 0, true);
    }
}
