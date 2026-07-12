use shared::{hue_dist, sat_cap};
use spacetimedb::rand::Rng;
use spacetimedb::{Identity, ReducerContext, ScheduleAt, Table, TimeDuration, Timestamp};
use std::collections::HashMap;

/// Canonical constants — see plan.md "Canonical constants" table. Mirror
/// these EXACTLY in the native and web clients. Anything genuinely shared
/// with the client (as opposed to server-only tuning) lives in the
/// `shared` crate and is re-exported here instead of hand-copied.
mod constants {
    pub use shared::constants::ISLAND_RADIUS;
    // Author-requested (F9.5): islands sit side by side, flat sides facing
    // flat sides (a perfect hex-of-hexes tiling — see
    // `geometry::SLOT_PLACEMENT_RADIUS`/`SLOT_U`/`SLOT_V`, which do the
    // actual placement math; a naive same-axis coarse scaling can never
    // reach zero gap at ANY spacing value, only shrink an always-triangular
    // gap toward it, because the coarse step directions point at the
    // hexagon's VERTICES rather than the middle of a flat edge), with a
    // deliberate uniform gap left between neighbors (not zero — author
    // wanted a little breathing room after all). See the `shared` crate's
    // doc comment for why it MUST be even.
    pub use shared::constants::MARGIN_GAP_TILES;
    // exactly the hexdist between any two ADJACENT islands' centers in that
    // tiling — verified computationally, not just algebra on paper.
    pub const SLOT_SPACING: i32 = 2 * (ISLAND_RADIUS + MARGIN_GAP_TILES / 2) + 1;
    // F9.5 item 3 (author-reported: "more range to merge" / known_bugs.md):
    // raised from 1.0 — the deployed build's range felt too short in
    // hand-testing. Mirrored in plan.md's constants table; re-tune both
    // together if the author's further hand-testing settles on a different
    // value.
    pub const MERGE_DIST: f32 = 1.5;
    // 50x the original plan.md values (1000 tiles / 20s sustained) — author
    // felt the original paint rate limit too restrictive in hand-testing.
    pub const PAINT_BUCKET_MAX: f32 = 1000.0;
    pub const PAINT_REFILL_PER_SEC: f32 = 50.0;
    pub const PRESENCE_TIMEOUT_SECS: i64 = 3;
    pub const XP_MERGE_NEW: u64 = 25;
    pub const XP_LIKE: u64 = 10;
    pub const XP_LINK_CLICK: u64 = 5;
    /// F9: passive XP for staying present (fresh `last_seen`), granted by
    /// `time_xp_tick` every `TIME_XP_PERIOD_SECS`.
    pub const XP_TIME: u64 = 1;
    pub const TIME_XP_PERIOD_SECS: i64 = 60;
    pub const LEVEL_XP: u64 = 100;
    /// Single source of truth in the `shared` crate — see its doc comment
    /// for why this is no longer hand-mirrored.
    pub use shared::constants::{START_SAT, START_VAL};
    /// F8: how often islands re-rank, and how long the client-facing
    /// countdown warns before slots actually get rewritten.
    pub const RERANK_PERIOD_SECS: i64 = 300;
    pub const RERANK_WARNING_SECS: i64 = 5;
    /// How far (degrees, either direction) a painted hue may stray from an
    /// unlocked inventory entry — lets the Hue slider nudge a shade without
    /// bloating the inventory with one row per nudge.
    pub use shared::constants::HUE_TOLERANCE;
    /// F9.5 item 10: how often the dead-player reap sweep runs, and how
    /// stale (`online == false` and `last_seen` older than this) a user must
    /// be before it's even considered a candidate.
    pub const REAP_PERIOD_SECS: i64 = 60;
    pub const REAP_IDLE_SECS: i64 = 300;
    /// A candidate is only actually reaped if their island has zero painted
    /// cells AND their inventory is at most this many rows (the seed hue —
    /// XP is deliberately not part of the test, since idle time-XP ticks may
    /// have granted a few by the time they're stale enough to qualify).
    pub const REAP_MAX_INVENTORY_ROWS: usize = 1;
    /// F10 (hexel.md's Admin section): SHA-256 of the admin password,
    /// baked in at compile time by `build.rs` from the `ADMIN_PASSWORD` env
    /// var / gitignored `.env` (see `server/.env.example`) — never the
    /// plaintext, since this repo is public. `claim_admin` hashes the
    /// caller's input and compares hex digests. To change the admin
    /// password: edit `.env`, rebuild, republish.
    pub const ADMIN_PASSWORD_SHA256: &str = env!("ADMIN_PASSWORD_SHA256");
    /// F11 (flying gift): SHARED with the client — `claim_gift`'s distance
    /// check and both clients' drift rendering must derive the identical
    /// position/range, so these three (unlike the tuning knobs above) live
    /// in the `shared` crate instead of being server-only.
    pub use shared::constants::{GIFT_CLAIM_DIST, GIFT_DRIFT_PERIOD_SECS, GIFT_DRIFT_RADIUS};
    /// F11: how often the spawn/expire tick runs, and how long an unclaimed
    /// gift lasts before that tick sweeps it away. Server-only — clients
    /// just observe `gift` rows appear/disappear, no client-side timer.
    /// `GIFT_LIFETIME_SECS` < `GIFT_SPAWN_PERIOD_SECS` so there's visible
    /// down-time between gifts rather than one always being up.
    pub const GIFT_SPAWN_PERIOD_SECS: i64 = 60;
    pub const GIFT_LIFETIME_SECS: i64 = 55;
    /// F11: flat XP on the claim coin-flip's "XP" branch — also the "hue"
    /// branch's own fallback if 8 random rerolls all collide with a hue the
    /// claimant already owns, so a win is never silently wasted.
    pub const XP_GIFT: u64 = 20;
    /// F13 (Hexa event): SHARED with the client — its rendering reads the
    /// server-broadcast `hexa_cluster` rows, which are keyed by this same
    /// detection radius/size, so these two (unlike the tuning knobs above)
    /// live in the `shared` crate. `XP_HEXA` is server-only but kept
    /// alongside them as the third F13 canonical constant.
    pub use shared::constants::{HEXA_RADIUS, HEXA_SIZE, HEXA_UNLOCK_LEVEL, XP_HEXA};
    /// F13: how often the `hexa_cluster` safety-net sweep runs — deletes a
    /// row whose owner went stale (offline, or `last_seen` past
    /// `PRESENCE_TIMEOUT_SECS`) without ever calling `set_pos` again to
    /// clear their own row (a departing/disconnecting member's row would
    /// otherwise linger, showing a hexagon that no longer really exists).
    pub const HEXA_SWEEP_PERIOD_SECS: i64 = 2;
}

/// Axial hex geometry, slot-lattice mapping and cell-id packing — see
/// plan.md "Geometry spec". Pure functions only; no DB access.
mod geometry {
    use super::constants::{ISLAND_RADIUS, MARGIN_GAP_TILES};

    /// Hex-of-hexes tiling basis (F9.5): the two coarse-lattice generators
    /// that tile hex-distance-`R` "super-hexagons" (`3R²+3R+1` cells each)
    /// with ZERO gap and ZERO overlap — a standard identity for
    /// centered-hexagonal-number clusters, verified computationally (every
    /// fine cell near the origin belongs to EXACTLY one placement cell)
    /// rather than assumed from the formula alone. `slot_coords` below is
    /// completely unaware of these — it only ever enumerates abstract
    /// integer (q, r) hex-ring positions; `SLOT_U`/`SLOT_V` are what turn
    /// that abstract index into an actual fine-grid position, in
    /// `slot_center`.
    ///
    /// Deliberately `ISLAND_RADIUS + MARGIN_GAP_TILES / 2`, not
    /// `ISLAND_RADIUS` itself: tiling on the ACTUAL island radius touches
    /// with zero gap at all (verified too, but the author wanted a little
    /// breathing room between islands after seeing it) — placing islands as
    /// if they were `MARGIN_GAP_TILES / 2` tiles bigger, while their real
    /// paintable interior (`ISLAND_RADIUS`, everywhere else in this file)
    /// stays unchanged, leaves a uniform `MARGIN_GAP_TILES`-tile gap on
    /// every side instead (the bump is shared symmetrically by both
    /// neighboring islands, so the gap is exactly `2 * bump` —
    /// `MARGIN_GAP_TILES` must stay even, see its doc comment). `R` here
    /// means this bumped placement radius, not `ISLAND_RADIUS`.
    const SLOT_PLACEMENT_RADIUS: i32 = ISLAND_RADIUS + MARGIN_GAP_TILES / 2;
    const SLOT_U: (i32, i32) = (SLOT_PLACEMENT_RADIUS, SLOT_PLACEMENT_RADIUS + 1);
    const SLOT_V: (i32, i32) = (-(SLOT_PLACEMENT_RADIUS + 1), 2 * SLOT_PLACEMENT_RADIUS + 1);
    /// Determinant of the [`SLOT_U`, `SLOT_V`] basis matrix — the
    /// denominator when inverting the map in `in_any_island_territory`. No
    /// longer equal to `ISLAND_RADIUS`'s cell count now that the placement
    /// radius is deliberately bumped for a gap (that identity only held at
    /// zero gap); still the reason this tiles perfectly regardless.
    const SLOT_DET: i32 = SLOT_U.0 * SLOT_V.1 - SLOT_U.1 * SLOT_V.0;

    pub fn hexdist(dq: i32, dr: i32) -> i32 {
        (dq.abs() + dr.abs() + (dq + dr).abs()) / 2
    }

    /// Fractional axial -> nearest integer axial (standard cube-round).
    pub fn cube_round(qf: f32, rf: f32) -> (i32, i32) {
        let sf = -qf - rf;
        let mut q = qf.round();
        let mut r = rf.round();
        let s = sf.round();
        let q_diff = (q - qf).abs();
        let r_diff = (r - rf).abs();
        let s_diff = (s - sf).abs();
        if q_diff > r_diff && q_diff > s_diff {
            q = -r - s;
        } else if r_diff > s_diff {
            r = -q - s;
        }
        (q as i32, r as i32)
    }

    /// E, SE, NW, W, SW, NE.
    pub const DIRECTIONS: [(i32, i32); 6] = [(1, 0), (1, -1), (0, -1), (-1, 0), (-1, 1), (0, 1)];

    fn ring_of(slot_index: u32) -> i32 {
        if slot_index == 0 {
            return 0;
        }
        let mut remaining = slot_index;
        let mut ring: i32 = 1;
        loop {
            let ring_size = (6 * ring) as u32;
            if remaining <= ring_size {
                return ring;
            }
            remaining -= ring_size;
            ring += 1;
        }
    }

    /// slot_index -> coarse axial (Q, R). Slot 0 = admin at the origin;
    /// slots 1.. spiral out over concentric coarse rings.
    pub fn slot_coords(slot_index: u32) -> (i32, i32) {
        if slot_index == 0 {
            return (0, 0);
        }
        let mut remaining = slot_index;
        let mut ring: i32 = 1;
        loop {
            let ring_size = (6 * ring) as u32;
            if remaining <= ring_size {
                break;
            }
            remaining -= ring_size;
            ring += 1;
        }
        let (sq, sr) = DIRECTIONS[4];
        let mut q = sq * ring;
        let mut r = sr * ring;
        let mut steps_left = remaining - 1;
        'outer: for &(dq, dr) in DIRECTIONS.iter() {
            for _ in 0..ring {
                if steps_left == 0 {
                    break 'outer;
                }
                q += dq;
                r += dr;
                steps_left -= 1;
            }
        }
        (q, r)
    }

    /// How many concentric coarse rings are needed to hold the slots in use
    /// so far, given the highest occupied slot index. Used to bound the
    /// margin canvas.
    pub fn occupied_rings(highest_slot: u32) -> i32 {
        ring_of(highest_slot).max(1)
    }

    /// Fine axial center of a coarse slot — `q * SLOT_U + r * SLOT_V` (a
    /// proper 2D linear combination of the tiling basis, NOT independent
    /// per-axis scaling — see `SLOT_U`/`SLOT_V`'s comment for why that
    /// distinction is exactly what eliminates the gaps).
    pub fn slot_center(q: i32, r: i32) -> (i32, i32) {
        (q * SLOT_U.0 + r * SLOT_V.0, q * SLOT_U.1 + r * SLOT_V.1)
    }

    /// True if the fine world cell `(q, r)` belongs to some island's
    /// interior (occupied or not — the whole coarse lattice is reserved).
    pub fn in_any_island_territory(q: i32, r: i32) -> bool {
        // Invert the SLOT_U/SLOT_V linear map — a real 2x2 matrix inverse,
        // not independent per-axis division, since the basis isn't
        // axis-aligned — to find the coarse slot this fine cell is nearest
        // to, then (unchanged from before) check that candidate AND its 6
        // neighbors, since a plain `cube_round` can land one cell off near a
        // boundary.
        let det = SLOT_DET as f32;
        let qf = (SLOT_V.1 as f32 * q as f32 - SLOT_V.0 as f32 * r as f32) / det;
        let rf = (-SLOT_U.1 as f32 * q as f32 + SLOT_U.0 as f32 * r as f32) / det;
        let (cq, cr) = cube_round(qf, rf);
        for &(dq, dr) in std::iter::once(&(0, 0)).chain(DIRECTIONS.iter()) {
            let (sq, sr) = (cq + dq, cr + dr);
            let (cx, cy) = slot_center(sq, sr);
            if hexdist(q - cx, r - cy) <= ISLAND_RADIUS {
                return true;
            }
        }
        false
    }

    pub fn island_cell_id(island_id: u32, q_local: i32, r_local: i32) -> u32 {
        (island_id << 10) | (((q_local + 16) as u32) << 5) | ((r_local + 16) as u32)
    }

    pub fn margin_cell_id(q: i32, r: i32) -> u32 {
        (((q + 512) as u32) << 10) | ((r + 512) as u32)
    }

    pub fn pack_hsv(h: u16, s: u8, v: u8) -> u32 {
        ((h as u32) << 16) | ((s as u32) << 8) | (v as u32)
    }
}

/// Circular-mean merge formula — see plan.md "Color spec". Needs `libm`
/// because wasm32-unknown-unknown's `std` does not link transcendental
/// float functions (sin/cos/atan2).
mod merge {
    pub fn merge_hue(h1: u16, h2: u16) -> u16 {
        debug_assert!(h1 != h2);
        let diff = (h1 as i32 - h2 as i32).rem_euclid(360);
        if diff == 180 {
            return ((h1.min(h2) as i32 + 90).rem_euclid(360)) as u16;
        }
        let to_rad = |d: u16| (d as f32) * std::f32::consts::PI / 180.0;
        let (s1, c1) = (libm::sinf(to_rad(h1)), libm::cosf(to_rad(h1)));
        let (s2, c2) = (libm::sinf(to_rad(h2)), libm::cosf(to_rad(h2)));
        let angle = libm::atan2f(s1 + s2, c1 + c2);
        let deg = angle * 180.0 / std::f32::consts::PI;
        (deg.round() as i32).rem_euclid(360) as u16
    }
}

/// F11: flying-gift world position — a small circular drift around the
/// spawn point, a pure function of elapsed seconds since `Gift.spawned_at`.
/// Mirrors `client/src/world.rs`'s `gift_drift_pos` exactly — both clients
/// must render/hit-test the SAME position from the same inputs (no
/// continuous position sync) — and `claim_gift` uses this, not the static
/// spawn point, as the "how far is the caller from the gift RIGHT NOW"
/// anti-cheat check. Needs `libm`, same reason `merge::merge_hue` does
/// (wasm32-unknown-unknown's `std` doesn't link sin/cos).
fn gift_drift_pos(spawn_x: f32, spawn_y: f32, elapsed_secs: f32) -> (f32, f32) {
    let angle = elapsed_secs / constants::GIFT_DRIFT_PERIOD_SECS * std::f32::consts::TAU;
    (
        spawn_x + constants::GIFT_DRIFT_RADIUS * libm::cosf(angle),
        spawn_y + constants::GIFT_DRIFT_RADIUS * libm::sinf(angle),
    )
}

#[spacetimedb::table(accessor = config, public)]
pub struct Config {
    #[primary_key]
    id: u32,
    #[default(false)]
    frozen: bool,
    #[default(None::<Identity>)]
    admin: Option<Identity>,
    /// F8: when set, a re-rank is landing at this timestamp — clients render
    /// a countdown banner from it. `None` outside the `RERANK_WARNING_SECS`
    /// window before a cycle fires.
    next_rerank_at: Option<Timestamp>,
}

#[spacetimedb::table(accessor = user, public)]
#[derive(Clone)]
pub struct User {
    #[primary_key]
    identity: Identity,
    name: Option<String>,
    online: bool,
    cx: f32,
    cy: f32,
    last_seen: Timestamp,
    hue: u16,
    sat: u8,
    val: u8,
    locked: bool,
    xp: u64,
    paint_tokens: f32,
    tokens_at: Timestamp,
}

#[spacetimedb::table(accessor = inventory, public)]
pub struct Inventory {
    #[primary_key]
    #[auto_inc]
    id: u64,
    #[index(btree)]
    owner: Identity,
    hue: u16,
    obtained_at: Timestamp,
    obtained_with: Option<Identity>,
    /// F11: true only for a flying-gift hue grant. Without this, the
    /// client's inventory-insert watch can't tell a gift-granted hue
    /// (`obtained_with: None`, same as a fresh row) apart from
    /// `reset_account`'s reseed, which it already treats specially (resets
    /// the last-3 color ring instead of showing a "new color" toast) —
    /// appended at the end, same reason `Island.border_color` was, so
    /// `bin/web.rs`'s positional row parsing doesn't shift.
    #[default(false)]
    from_gift: bool,
}

#[spacetimedb::table(accessor = island, public)]
#[derive(Clone)]
pub struct Island {
    #[primary_key]
    #[auto_inc]
    id: u32,
    #[unique]
    owner: Identity,
    #[unique]
    slot: u32,
    likes: u32,
    itch_rate_id: Option<u32>,
    created_at: Timestamp,
    /// Author-requested: lets an owner pin their island's border to a
    /// specific packed HSV color instead of always tracking their seed hue.
    /// `None` = fall back to the seed-hue default clients already render.
    /// New fields appended at the end (not inserted among the existing
    /// ones) so `bin/web.rs`'s hand-rolled positional row parsing, which
    /// indexes fields by schema order, doesn't shift under it.
    #[default(None::<u32>)]
    border_color: Option<u32>,
    /// Author-requested: hides the border entirely regardless of
    /// `border_color` — a separate flag rather than overloading
    /// `border_color: None` for "hidden", since that value already means
    /// "use the default seed-hue color".
    #[default(false)]
    border_hidden: bool,
}

/// F8: one row per (island, liker) — enforced in `like_island` rather than as
/// a DB-level compound unique constraint (this SDK only supports uniqueness
/// on a single column).
#[spacetimedb::table(accessor = island_like, public)]
pub struct IslandLike {
    #[primary_key]
    #[auto_inc]
    id: u64,
    #[index(btree)]
    island_id: u32,
    liker: Identity,
}

/// F9: one row per (island, clicker) — same dedupe pattern as `IslandLike`,
/// enforced in `click_link` rather than a DB-level compound unique
/// constraint. No "unclick": a link click's XP credit is permanent, unlike a
/// like.
#[spacetimedb::table(accessor = island_link_click, public)]
pub struct IslandLinkClick {
    #[primary_key]
    #[auto_inc]
    id: u64,
    #[index(btree)]
    island_id: u32,
    clicker: Identity,
}

/// F8 re-rank timer chain: a repeating loop schedules the "warning" step
/// every `RERANK_PERIOD_SECS`; the warning step sets `config.next_rerank_at`
/// (for the client countdown) and schedules a ONE-SHOT fire step
/// `RERANK_WARNING_SECS` later, which does the actual re-sort. Both tables
/// are server-internal (not `public`) — clients only ever see their effect
/// through `config.next_rerank_at` and `island.slot`.
#[spacetimedb::table(accessor = rerank_warn_schedule, scheduled(rerank_warn))]
pub struct RerankWarnSchedule {
    #[primary_key]
    #[auto_inc]
    scheduled_id: u64,
    scheduled_at: ScheduleAt,
}

#[spacetimedb::table(accessor = rerank_fire_schedule, scheduled(rerank_fire))]
pub struct RerankFireSchedule {
    #[primary_key]
    #[auto_inc]
    scheduled_id: u64,
    scheduled_at: ScheduleAt,
}

/// F9: repeating tick (see `client_connected`'s lazy seeding, same pattern as
/// `rerank_warn_schedule`) that grants `XP_TIME` to every present user every
/// `TIME_XP_PERIOD_SECS`. Server-internal, not `public` — clients only ever
/// observe its effect through `user.xp`.
#[spacetimedb::table(accessor = time_xp_schedule, scheduled(time_xp_tick))]
pub struct TimeXpSchedule {
    #[primary_key]
    #[auto_inc]
    scheduled_id: u64,
    scheduled_at: ScheduleAt,
}

/// F9.5 item 10: repeating tick (same lazy-seeding pattern as
/// `time_xp_schedule`) that reaps drive-by players — see `reap_dead_players`.
/// Server-internal, not `public`; clients only ever observe its effect
/// through `user`/`island` rows disappearing.
#[spacetimedb::table(accessor = reap_schedule, scheduled(reap_dead_players))]
pub struct ReapSchedule {
    #[primary_key]
    #[auto_inc]
    scheduled_id: u64,
    scheduled_at: ScheduleAt,
}

#[spacetimedb::table(accessor = island_cell, public)]
pub struct IslandCell {
    #[primary_key]
    id: u32,
    #[index(btree)]
    island_id: u32,
    q: i32,
    r: i32,
    color: u32,
    painted_by: Identity,
    painted_at: Timestamp,
}

#[spacetimedb::table(accessor = margin_cell, public)]
pub struct MarginCell {
    #[primary_key]
    id: u32,
    q: i32,
    r: i32,
    color: u32,
    painted_by: Identity,
    painted_at: Timestamp,
}

/// F11 (flying gift): one row = the single currently-active pickup in the
/// world — `gift_tick` only ever spawns a new one once the current row is
/// gone (at most one at a time, the pragmatic P2 scope call). `x`/`y` are
/// the world-cartesian SPAWN center; the actual on-screen position drifts
/// from it over time (`gift_drift_pos`, mirrored client-side) rather than
/// being pushed continuously. `expires_at` is what `claim_gift` re-checks
/// at call time too, in case a claim lands the same tick that would have
/// expired it.
#[spacetimedb::table(accessor = gift, public)]
pub struct Gift {
    #[primary_key]
    #[auto_inc]
    id: u64,
    x: f32,
    y: f32,
    spawned_at: Timestamp,
    expires_at: Timestamp,
}

/// F11: repeating tick (same lazy-seeding pattern as the schedules above)
/// that expires stale gifts and spawns a fresh one when none remain.
/// Server-internal, not `public` — clients only ever observe its effect
/// through `gift` rows appearing/disappearing.
#[spacetimedb::table(accessor = gift_schedule, scheduled(gift_tick))]
pub struct GiftSchedule {
    #[primary_key]
    #[auto_inc]
    scheduled_id: u64,
    scheduled_at: ScheduleAt,
}

/// F13 (Hexa event): who has ever received the one-time `XP_HEXA` bonus —
/// `identity` as the primary key doubles as the "already granted?" index,
/// same trick `IslandLike`'s per-(island, liker) dedup uses one level up.
/// Server-internal bookkeeping only (not `public`): clients only ever
/// observe its effect through `User.xp` and `Inventory` rows, same as every
/// other XP grant in this file.
#[spacetimedb::table(accessor = hexa_reward)]
pub struct HexaReward {
    #[primary_key]
    identity: Identity,
    at: Timestamp,
}

/// F13: one row per ignition, purely so clients can animate it (flash the
/// hexagon edges, play the sfx) — `public` for that reason, unlike
/// `HexaReward` above. `cx`/`cy` are the fixed world origin and
/// `member_count` records the six occupants. Also doubles as the "was this `None`-obtained-with
/// `Inventory` row a Hexa grant, not a `reset_account` reseed?" join key for
/// the client's existing inventory-insert watch — see `apply_hexa`'s
/// comment on why that's a timestamp join rather than a new `Inventory`
/// field.
#[spacetimedb::table(accessor = hexa_event, public)]
pub struct HexaEvent {
    #[primary_key]
    #[auto_inc]
    id: u64,
    at: Timestamp,
    cx: f32,
    cy: f32,
    member_count: u32,
}

/// F13 author follow-up: the server broadcasts LIVE cluster-forming state
/// (not just the ignition moment `HexaEvent` logs) — "the server would tell
/// that there is an HEXA happening and give an id and position to players
/// so that everyone sees they are merging." One row per CURRENTLY-clustered
/// user occupying one of the six fixed world-origin slots. Level-3+
/// unlocked players within `HEXA_RADIUS` qualify regardless of hue. It is
/// `public` so every client renders the same authoritative formation.
/// `cluster_id` is always zero because only this one central formation can
/// exist. `cx`/`cy` is always the world origin;
/// `vertex_index` (0..=5) is which
/// hexagon-vertex slot this member renders at, so clients don't need to
/// re-derive vertex assignment themselves. It is STICKY, not sorted by
/// identity: a returning member keeps its prior seat across an unrelated
/// join/leave, and only a fresh joiner gets placed by real-world angle (see
/// `refresh_central_hexa`/`hexa_assign_seats`) — so it is stateful-but-
/// consistent (reducers serialize, so there's no cross-call race).
/// `ignited` mirrors `member_count >= HEXA_SIZE`.
#[spacetimedb::table(accessor = hexa_cluster, public)]
pub struct HexaCluster {
    #[primary_key]
    identity: Identity,
    cluster_id: u64,
    cx: f32,
    cy: f32,
    member_count: u32,
    vertex_index: u32,
    ignited: bool,
}

/// F13: repeating safety-net sweep (see `HEXA_SWEEP_PERIOD_SECS`'s doc
/// comment) — server-internal, not `public`, same as every other schedule
/// table here.
#[spacetimedb::table(accessor = hexa_sweep_schedule, scheduled(hexa_sweep))]
pub struct HexaSweepSchedule {
    #[primary_key]
    #[auto_inc]
    scheduled_id: u64,
    scheduled_at: ScheduleAt,
}

/// One row per cursor merge only (never a tile merge or eyedrop,
/// which has no second live player to show) — purely so clients can render
/// BOTH pre-merge hues alongside the result, which the existing
/// `Inventory`/`obtained_with` fields can't: by the time a client sees the
/// new `Inventory` row, `apply_merge` has already overwritten both players'
/// `User.hue` to the merged value, so the "before" hues are gone from live
/// state. `public`, same reasoning as `HexaEvent`. Told apart from a
/// `reset_account` reseed the same way `HexaEvent` is: joined against
/// `Inventory.obtained_at` on this row's `at`, both stamped with the same
/// `ctx.timestamp` by the same reducer call.
#[spacetimedb::table(accessor = merge_event, public)]
pub struct MergeEvent {
    #[primary_key]
    #[auto_inc]
    id: u64,
    at: Timestamp,
    a: Identity,
    b: Identity,
    hue_a: u16,
    hue_b: u16,
    merged_hue: u16,
    #[default(0)]
    merged_sat: u8,
    #[default(0)]
    merged_val: u8,
}

/// Author decision (F6 follow-up): the merge toast used to fall back to a
/// partner's short identity hex when they hadn't picked a name, which leaks
/// enough of the identity to correlate a player across merges — the same
/// identifier decision 11 requires to stay confidential. Auto-assigning a
/// name at first connect (rather than leaving `name: None`) means the
/// clients' identity-hex fallback essentially never fires in practice.
const NAME_ADJECTIVES: [&str; 20] = [
    "Swift", "Calm", "Bold", "Wild", "Bright", "Quiet", "Lucky", "Sunny", "Cosmic", "Golden",
    "Silver", "Amber", "Coral", "Azure", "Violet", "Crimson", "Emerald", "Ivory", "Jade", "Rusty",
];
const NAME_NOUNS: [&str; 20] = [
    "Fox", "Otter", "Hex", "Reef", "Island", "Wren", "Falcon", "Panda", "Comet", "Nova", "Pixel",
    "Dune", "Tide", "Ember", "Lynx", "Heron", "Wisp", "Atoll", "Drifter", "Compass",
];

fn random_name(ctx: &ReducerContext) -> String {
    // `&StdbRng`'s `RngCore` impl needs `&mut self`, so the binding itself
    // must be `mut` here (unlike a one-shot `ctx.rng().gen_range(..)` call,
    // which mutably borrows the temporary automatically).
    let mut rng = ctx.rng();
    let a = NAME_ADJECTIVES[rng.gen_range(0..NAME_ADJECTIVES.len())];
    let n = NAME_NOUNS[rng.gen_range(0..NAME_NOUNS.len())];
    format!("{a}{n}")
}

/// F9.5 (dead-player reap): smallest slot `>= 1` not currently held by a
/// live island — slot 0 is reserved for the admin island, never assigned
/// here. `O(n log n)` in the current island count, called only on connect
/// (a rare event, not a hot per-frame path), so the sort is cheap enough not
/// to warrant a smarter free-list structure.
fn lowest_free_slot(ctx: &ReducerContext) -> u32 {
    let mut used: Vec<u32> = ctx.db.island().iter().map(|i| i.slot).collect();
    used.sort_unstable();
    let mut candidate = 1u32;
    for slot in used {
        if slot == candidate {
            candidate += 1;
        } else if slot > candidate {
            break;
        }
    }
    candidate
}

fn level_of(xp: u64) -> u64 {
    xp / constants::LEVEL_XP
}

/// Deterministic-enough starting hue: hashes the identity's bytes. Not
/// cryptographic, just needs to spread new players across the wheel.
fn start_hue(identity: &Identity) -> u16 {
    let h = identity
        .to_byte_array()
        .iter()
        .fold(7u32, |acc, b| acc.wrapping_mul(31).wrapping_add(*b as u32));
    (h % 360) as u16
}

fn refill_tokens(user: &User, now: Timestamp) -> f32 {
    let elapsed_s = now
        .duration_since(user.tokens_at)
        .map(|d| d.as_secs_f32())
        .unwrap_or(0.0)
        .max(0.0);
    (user.paint_tokens + elapsed_s * constants::PAINT_REFILL_PER_SEC).min(constants::PAINT_BUCKET_MAX)
}

/// Shared by both paint reducers: checks the caller exists, refills +
/// spends one token, and returns the row to re-save plus the caller's
/// current brush color. Colors are never taken from the client — this is
/// the single anti-cheat point for painted color.
fn take_paint_token(ctx: &ReducerContext) -> Result<(User, u32), String> {
    let user = ctx
        .db
        .user()
        .identity()
        .find(ctx.sender())
        .ok_or("unknown user")?;
    let tokens = refill_tokens(&user, ctx.timestamp);
    if tokens < 1.0 {
        return Err("rate limited".to_string());
    }
    let color = geometry::pack_hsv(user.hue, user.sat, user.val);
    let user = User {
        paint_tokens: tokens - 1.0,
        tokens_at: ctx.timestamp,
        ..user
    };
    ctx.db.user().identity().update(user.clone());
    Ok((user, color))
}

/// F10: hex-encoded SHA-256 digest of `password`, compared against
/// `constants::ADMIN_PASSWORD_SHA256` by `claim_admin`.
fn hash_password(password: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(password.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// F10: shared guard for `set_frozen`/`delete_island_cells` — both are
/// admin-only, not gated by `check_not_frozen` below (the admin must still
/// be able to act, in particular to unfreeze, while the game is frozen).
fn require_admin(ctx: &ReducerContext) -> Result<(), String> {
    let config = ctx.db.config().id().find(0).ok_or("config not initialized")?;
    if config.admin != Some(ctx.sender()) {
        return Err("admin only".to_string());
    }
    Ok(())
}

/// F10 (hexel.md's Admin section: "can freeze the game so no one can
/// interact anymore"): shared guard called first by every player-facing
/// mutating reducer. Scheduled/system reducers (rerank, time-XP, reap,
/// connect/disconnect) and the three admin reducers deliberately do NOT call
/// this — freeze stops PLAYER interaction, not the world's background clocks
/// or the admin's own tools.
fn check_not_frozen(ctx: &ReducerContext) -> Result<(), String> {
    if ctx.db.config().id().find(0).is_some_and(|c| c.frozen) {
        return Err("the game is frozen".to_string());
    }
    Ok(())
}

#[spacetimedb::reducer]
pub fn set_pos(ctx: &ReducerContext, cx: f32, cy: f32) -> Result<(), String> {
    check_not_frozen(ctx)?;
    let user = ctx
        .db
        .user()
        .identity()
        .find(ctx.sender())
        .ok_or("unknown user")?;
    let caller_hue = user.hue;
    let caller = ctx.sender();
    ctx.db.user().identity().update(User {
        cx,
        cy,
        last_seen: ctx.timestamp,
        ..user
    });

    // Cursor-merge detection: nearest unlocked, sufficiently-different online user
    // within MERGE_DIST. Deliberately NOT gated on `last_seen` freshness
    // (unlike HEXA eligibility/time-XP below) — the client only sends
    // `set_pos` when the mouse moves, so a player standing still for more
    // than PRESENCE_TIMEOUT_SECS would otherwise become un-mergeable despite
    // being visibly online and in place. `online` (reliably cleared on
    // disconnect) is enough to know they're really there.
    let mut nearest: Option<(Identity, u16, f32)> = None;
    for other in ctx.db.user().iter() {
        if other.identity == caller
            || !other.online
            || other.locked
            || hue_dist(caller_hue, other.hue) <= constants::HUE_TOLERANCE
        {
            continue;
        }
        let d2 = (other.cx - cx).powi(2) + (other.cy - cy).powi(2);
        if d2 >= constants::MERGE_DIST * constants::MERGE_DIST {
            continue;
        }
        if nearest.is_none_or(|(_, _, best)| d2 < best) {
            nearest = Some((other.identity, other.hue, d2));
        }
    }

    if let Some((partner, partner_hue, _)) = nearest {
        let me = ctx.db.user().identity().find(caller).ok_or("unknown user")?;
        if !me.locked && hue_dist(me.hue, partner_hue) > constants::HUE_TOLERANCE {
            let partner_user = ctx.db.user().identity().find(partner).ok_or("unknown partner")?;
            let merged = merge::merge_hue(me.hue, partner_hue);
            // Keep the shared result identical for both players. Using the
            // lower current level cap still blends saturation, while avoiding
            // an event color that one participant cannot actually select.
            let merged_sat = (((me.sat as u16 + partner_user.sat as u16 + 1) / 2) as u8)
                .min(sat_cap(level_of(me.xp)))
                .min(sat_cap(level_of(partner_user.xp)));
            let merged_val = ((me.val as u16 + partner_user.val as u16 + 1) / 2) as u8;
            ctx.db.merge_event().insert(MergeEvent {
                id: 0,
                at: ctx.timestamp,
                a: caller,
                b: partner,
                hue_a: me.hue,
                hue_b: partner_hue,
                merged_hue: merged,
                merged_sat,
                merged_val,
            });
            apply_merge(ctx, caller, partner, merged, merged_sat, merged_val, true);
        }
    }

    // HEXA is deliberately separate from ordinary cursor merging. Level-3+
    // unlocked players near the world origin occupy the nearest free fixed
    // slot, regardless of hue or proximity to one another.
    refresh_central_hexa(ctx);
    Ok(())
}

/// Fixed world-angle (radians) of hexagon slot `s` —
/// slot 0 straight up, `+FRAC_PI_3` per slot going around, matching the
/// client's `hexagon_vertex_positions` layout at phase 0 exactly (both sides
/// must agree on which physical direction "slot 2" points, since the client
/// draws the vertex and this only picks the index).
fn hexa_slot_angle(s: u32) -> f32 {
    -std::f32::consts::FRAC_PI_2 + (s % 6) as f32 * std::f32::consts::FRAC_PI_3
}

/// Wrap-aware angular distance in radians, always in `[0, PI]` (e.g. the gap
/// between angle `-PI + 0.1` and `PI - 0.1` is `0.2`, not `2*PI - 0.2`).
fn hexa_ang_dist(a: f32, b: f32) -> f32 {
    let diff = (a - b).rem_euclid(std::f32::consts::TAU);
    diff.min(std::f32::consts::TAU - diff)
}

/// Sticky angle-based seat assignment for the six fixed slots at the world
/// origin. `prior`/`angles` use the same member order; returning occupants
/// retain their seat and a newcomer takes the free slot closest to the
/// direction from which they approached the centre.
///
/// Pass 1: every returning member keeps `prior % 6`. If two returning
/// members collide on the same mod-6 slot (only possible right after a
/// cluster MERGE, where two previously-separate hexagons' seat numbering
/// overlaps), the first one in identity order keeps it; the loser falls
/// through to pass 2 and gets reseated like a fresh join.
/// Pass 2: every unseated member takes whichever FREE slot (0..6) has the
/// fixed angle closest to their actual approach angle around the origin —
/// ties favor the lowest index, for determinism. This is what makes a new
/// joiner slot in next to where they physically are, instead of an
/// arbitrary identity-sort position.
fn hexa_assign_seats(prior: &[Option<u32>], angles: &[f32]) -> Vec<u32> {
    let n = prior.len();
    let mut seats: Vec<Option<u32>> = vec![None; n];
    let mut taken: std::collections::HashSet<u32> = std::collections::HashSet::new();

    for i in 0..n {
        if let Some(p) = prior[i] {
            let slot = p % 6;
            if taken.insert(slot) {
                seats[i] = Some(slot);
            }
        }
    }

    for i in 0..n {
        if seats[i].is_some() {
            continue;
        }
        let mut best: Option<(u32, f32)> = None;
        for slot in 0..6u32 {
            if taken.contains(&slot) {
                continue;
            }
            let d = hexa_ang_dist(angles[i], hexa_slot_angle(slot));
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some((slot, d));
            }
        }
        let slot = best.expect("central HEXA is capped at six occupants").0;
        taken.insert(slot);
        seats[i] = Some(slot);
    }

    seats.into_iter().map(|s| s.unwrap()).collect()
}

/// Rebuilds the one world-centre HEXA formation from live eligible players.
/// Existing occupants get first refusal on their seats; remaining places go
/// to the nearest waiting cursors. At most six rows exist, so a full HEXA
/// never overlaps a seventh cursor.
fn refresh_central_hexa(ctx: &ReducerContext) {
    let prior_ignited: std::collections::HashSet<Identity> = ctx
        .db
        .hexa_cluster()
        .iter()
        .filter(|row| row.ignited)
        .map(|row| row.identity)
        .collect();
    let radius2 = constants::HEXA_RADIUS * constants::HEXA_RADIUS;
    let mut eligible: Vec<(Identity, f32, f32, Option<u32>)> = ctx
        .db
        .user()
        .iter()
        .filter(|u| {
            u.online
                && !u.locked
                && level_of(u.xp) >= constants::HEXA_UNLOCK_LEVEL
                && u.cx * u.cx + u.cy * u.cy < radius2
                && ctx
                    .timestamp
                    .duration_since(u.last_seen)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(i64::MAX)
                    < constants::PRESENCE_TIMEOUT_SECS
        })
        .map(|u| {
            let prior = ctx.db.hexa_cluster().identity().find(u.identity).map(|row| row.vertex_index);
            (u.identity, u.cx, u.cy, prior)
        })
        .collect();

    eligible.sort_by(|a, b| {
        b.3.is_some()
            .cmp(&a.3.is_some())
            .then_with(|| (a.1 * a.1 + a.2 * a.2).total_cmp(&(b.1 * b.1 + b.2 * b.2)))
            .then_with(|| a.0.cmp(&b.0))
    });
    eligible.truncate(constants::HEXA_SIZE);
    eligible.sort_by_key(|&(id, _, _, _)| id);

    let selected: std::collections::HashSet<Identity> = eligible.iter().map(|&(id, _, _, _)| id).collect();
    let departed: Vec<Identity> = ctx.db.hexa_cluster().iter().filter(|row| !selected.contains(&row.identity)).map(|row| row.identity).collect();
    for identity in departed {
        ctx.db.hexa_cluster().identity().delete(identity);
    }

    if eligible.is_empty() {
        return;
    }

    let sorted: Vec<(Identity, f32, f32)> = eligible.iter().map(|&(id, x, y, _)| (id, x, y)).collect();
    let ids: Vec<Identity> = sorted.iter().map(|&(id, _, _)| id).collect();
    let ignited = sorted.len() >= constants::HEXA_SIZE;
    let new_ignition = ignited && prior_ignited != selected;

    let prior: Vec<Option<u32>> = eligible.iter().map(|row| row.3).collect();
    let angles: Vec<f32> = sorted.iter().map(|&(_, x, y)| libm::atan2f(y, x)).collect();
    let seats = hexa_assign_seats(&prior, &angles);

    for (i, &identity) in ids.iter().enumerate() {
        let row = HexaCluster {
            identity,
            cluster_id: 0,
            cx: 0.0,
            cy: 0.0,
            member_count: sorted.len() as u32,
            vertex_index: seats[i],
            ignited,
        };
        if ctx.db.hexa_cluster().identity().find(identity).is_some() {
            ctx.db.hexa_cluster().identity().update(row);
        } else {
            ctx.db.hexa_cluster().insert(row);
        }
    }
    if ignited {
        apply_hexa(ctx, &ids, 0.0, 0.0, new_ignition);
    }
}

/// Shared merge procedure for both cursor-merge and tile-merge. `cursor`
/// selects whether both sides' brush hue is updated (cursor-merge) or only
/// the caller's (tile-merge).
fn apply_merge(
    ctx: &ReducerContext,
    a: Identity,
    b: Identity,
    merged_hue: u16,
    merged_sat: u8,
    merged_val: u8,
    cursor: bool,
) {
    for &who in &[a, b] {
        let already_has = ctx
            .db
            .inventory()
            .owner()
            .filter(&who)
            .any(|inv| hue_dist(inv.hue, merged_hue) <= constants::HUE_TOLERANCE);
        if !already_has {
            let partner = if who == a { b } else { a };
            ctx.db.inventory().insert(Inventory {
                id: 0,
                owner: who,
                hue: merged_hue,
                obtained_at: ctx.timestamp,
                obtained_with: Some(partner),
                from_gift: false,
            });
            if let Some(u) = ctx.db.user().identity().find(who) {
                ctx.db.user().identity().update(User {
                    xp: u.xp + constants::XP_MERGE_NEW,
                    ..u
                });
            }
        }
    }
    if cursor {
        for &who in &[a, b] {
            if let Some(u) = ctx.db.user().identity().find(who) {
                let sat = merged_sat.min(sat_cap(level_of(u.xp)));
                ctx.db.user().identity().update(User { hue: merged_hue, sat, val: merged_val, ..u });
            }
        }
    } else if let Some(u) = ctx.db.user().identity().find(a) {
        let sat = merged_sat.min(sat_cap(level_of(u.xp)));
        ctx.db.user().identity().update(User { hue: merged_hue, sat, val: merged_val, ..u });
    }
}

/// F13 (Hexa event): pools every participant's owned hues (union, granted to
/// whoever's missing it) and grants `XP_HEXA` to any participant who's never
/// received it. Pooling is idempotent for a fixed group — re-running this on
/// a cluster that's already fully pooled and rewarded grants nothing new —
/// so `set_pos` calling it on every tick a cluster holds needs no cooldown
/// (author-confirmed design). `force_event` is true only when a new six-player
/// formation ignites, so repeat passes over the same formation do not spam
/// events while a later re-formation still gets its own popup. Granted rows use
/// `obtained_with: None` (same as a fresh seed hue or a `reset_account`
/// reseed) — the client tells them apart from those by joining
/// `Inventory.obtained_at` against this same call's `HexaEvent.at` (both
/// stamped with the same `ctx.timestamp`), rather than a fourth `Inventory`
/// meaning needing its own dedicated bool field.
fn apply_hexa(ctx: &ReducerContext, participants: &[Identity], cx: f32, cy: f32, force_event: bool) {
    let mut union_hues: Vec<u16> = Vec::new();
    for &p in participants {
        union_hues.extend(ctx.db.inventory().owner().filter(&p).map(|inv| inv.hue));
    }
    union_hues.sort_unstable();
    union_hues.dedup();

    let mut changed = false;
    for &who in participants {
        for &hue in &union_hues {
            let already_has = ctx.db.inventory().owner().filter(&who).any(|inv| inv.hue == hue);
            if !already_has {
                ctx.db.inventory().insert(Inventory {
                    id: 0,
                    owner: who,
                    hue,
                    obtained_at: ctx.timestamp,
                    obtained_with: None,
                    from_gift: false,
                });
                changed = true;
            }
        }
        if ctx.db.hexa_reward().identity().find(who).is_none() {
            ctx.db.hexa_reward().insert(HexaReward { identity: who, at: ctx.timestamp });
            if let Some(u) = ctx.db.user().identity().find(who) {
                ctx.db.user().identity().update(User { xp: u.xp + constants::XP_HEXA, ..u });
            }
            changed = true;
        }
    }

    if changed || force_event {
        ctx.db.hexa_event().insert(HexaEvent { id: 0, at: ctx.timestamp, cx, cy, member_count: participants.len() as u32 });
    }
}

#[spacetimedb::reducer]
pub fn set_name(ctx: &ReducerContext, name: String) -> Result<(), String> {
    check_not_frozen(ctx)?;
    if name.is_empty() {
        return Err("Names must not be empty".to_string());
    }
    let user = ctx.db.user().identity().find(ctx.sender()).ok_or("unknown user")?;
    ctx.db.user().identity().update(User { name: Some(name), ..user });
    Ok(())
}

#[spacetimedb::reducer]
pub fn set_lock(ctx: &ReducerContext, locked: bool) -> Result<(), String> {
    check_not_frozen(ctx)?;
    let user = ctx.db.user().identity().find(ctx.sender()).ok_or("unknown user")?;
    ctx.db.user().identity().update(User { locked, ..user });
    // Lock changes HEXA eligibility immediately; rebuild all rows so the
    // remaining occupants also see the updated member count.
    refresh_central_hexa(ctx);
    Ok(())
}

#[spacetimedb::reducer]
pub fn set_brush(ctx: &ReducerContext, hue: u16, sat: u8, val: u8) -> Result<(), String> {
    check_not_frozen(ctx)?;
    let user = ctx.db.user().identity().find(ctx.sender()).ok_or("unknown user")?;
    if hue > 359 {
        return Err("hue out of range".to_string());
    }
    if !ctx
        .db
        .inventory()
        .owner()
        .filter(&ctx.sender())
        .any(|inv| hue_dist(inv.hue, hue) <= constants::HUE_TOLERANCE)
    {
        return Err("hue not unlocked".to_string());
    }
    if sat > sat_cap(level_of(user.xp)) {
        return Err("saturation exceeds level cap".to_string());
    }
    if val > 100 {
        return Err("value out of range".to_string());
    }
    ctx.db.user().identity().update(User { hue, sat, val, ..user });
    Ok(())
}

#[spacetimedb::reducer]
pub fn paint_island_cell(ctx: &ReducerContext, q_local: i32, r_local: i32) -> Result<(), String> {
    check_not_frozen(ctx)?;
    if geometry::hexdist(q_local, r_local) > constants::ISLAND_RADIUS {
        return Err("cell is outside the island".to_string());
    }
    let island = ctx
        .db
        .island()
        .owner()
        .find(ctx.sender())
        .ok_or("you do not own an island")?;
    let (_, color) = take_paint_token(ctx)?;
    let id = geometry::island_cell_id(island.id, q_local, r_local);
    if let Some(cell) = ctx.db.island_cell().id().find(id) {
        ctx.db.island_cell().id().update(IslandCell {
            color,
            painted_by: ctx.sender(),
            painted_at: ctx.timestamp,
            ..cell
        });
    } else {
        ctx.db.island_cell().insert(IslandCell {
            id,
            island_id: island.id,
            q: q_local,
            r: r_local,
            color,
            painted_by: ctx.sender(),
            painted_at: ctx.timestamp,
        });
    }
    Ok(())
}

#[spacetimedb::reducer]
pub fn paint_margin_cell(ctx: &ReducerContext, q: i32, r: i32) -> Result<(), String> {
    check_not_frozen(ctx)?;
    if geometry::in_any_island_territory(q, r) {
        return Err("cell belongs to an island".to_string());
    }
    // F9.5 (dead-player reap): was `ctx.db.island().count()`, which only
    // equalled the highest live slot number while slots were never reused —
    // once reaping can delete an island out of the middle of the sequence,
    // `count()` under-reports the true highest slot still in use, which
    // would shrink the margin bound below cells that legitimately border a
    // still-live high-numbered island. Use the actual max.
    let highest_slot = ctx.db.island().iter().map(|i| i.slot).max().unwrap_or(0);
    let bound = constants::SLOT_SPACING * (geometry::occupied_rings(highest_slot) + 1);
    if geometry::hexdist(q, r) > bound {
        return Err("cell is outside the current canvas bounds".to_string());
    }
    let (_, color) = take_paint_token(ctx)?;
    let id = geometry::margin_cell_id(q, r);
    if let Some(cell) = ctx.db.margin_cell().id().find(id) {
        ctx.db.margin_cell().id().update(MarginCell {
            color,
            painted_by: ctx.sender(),
            painted_at: ctx.timestamp,
            ..cell
        });
    } else {
        ctx.db.margin_cell().insert(MarginCell {
            id,
            q,
            r,
            color,
            painted_by: ctx.sender(),
            painted_at: ctx.timestamp,
        });
    }
    Ok(())
}

/// F9.6 item 1 (eraser, decision 18): same validation as `paint_island_cell`
/// (radius bound + ownership), same token charge — an erase costs a paint
/// token just like a paint does, so it can't be used to bypass the rate
/// limit. Deleting a cell that was never painted is a harmless no-op (the
/// token is still spent, matching how re-painting an already-painted cell
/// also still spends one).
#[spacetimedb::reducer]
pub fn erase_island_cell(ctx: &ReducerContext, q_local: i32, r_local: i32) -> Result<(), String> {
    check_not_frozen(ctx)?;
    if geometry::hexdist(q_local, r_local) > constants::ISLAND_RADIUS {
        return Err("cell is outside the island".to_string());
    }
    let island = ctx
        .db
        .island()
        .owner()
        .find(ctx.sender())
        .ok_or("you do not own an island")?;
    take_paint_token(ctx)?;
    let id = geometry::island_cell_id(island.id, q_local, r_local);
    ctx.db.island_cell().id().delete(id);
    Ok(())
}

/// F14 (decision 20, 2026-07-11): the community island at slot 0 — same
/// bound check and paint-token charge as `paint_island_cell`, but with NO
/// ownership check, since it belongs to everyone. `client_connected`
/// guarantees the slot-0 row exists before any client could plausibly reach
/// here (`ok_or` is defensive, not expected to fire).
#[spacetimedb::reducer]
pub fn paint_community_cell(ctx: &ReducerContext, q_local: i32, r_local: i32) -> Result<(), String> {
    check_not_frozen(ctx)?;
    if geometry::hexdist(q_local, r_local) > constants::ISLAND_RADIUS {
        return Err("cell is outside the island".to_string());
    }
    let island = ctx.db.island().slot().find(0).ok_or("community island not initialized")?;
    let (_, color) = take_paint_token(ctx)?;
    let id = geometry::island_cell_id(island.id, q_local, r_local);
    if let Some(cell) = ctx.db.island_cell().id().find(id) {
        ctx.db.island_cell().id().update(IslandCell {
            color,
            painted_by: ctx.sender(),
            painted_at: ctx.timestamp,
            ..cell
        });
    } else {
        ctx.db.island_cell().insert(IslandCell {
            id,
            island_id: island.id,
            q: q_local,
            r: r_local,
            color,
            painted_by: ctx.sender(),
            painted_at: ctx.timestamp,
        });
    }
    Ok(())
}

/// F14 (decision 20, 2026-07-11): erase counterpart to `paint_community_cell`
/// — same relationship `erase_island_cell` has to `paint_island_cell`.
#[spacetimedb::reducer]
pub fn erase_community_cell(ctx: &ReducerContext, q_local: i32, r_local: i32) -> Result<(), String> {
    check_not_frozen(ctx)?;
    if geometry::hexdist(q_local, r_local) > constants::ISLAND_RADIUS {
        return Err("cell is outside the island".to_string());
    }
    let island = ctx.db.island().slot().find(0).ok_or("community island not initialized")?;
    take_paint_token(ctx)?;
    let id = geometry::island_cell_id(island.id, q_local, r_local);
    ctx.db.island_cell().id().delete(id);
    Ok(())
}

/// F9.6 item 1 (eraser, decision 18): same validation as `paint_margin_cell`
/// (canvas bound, not island territory), same token charge.
#[spacetimedb::reducer]
pub fn erase_margin_cell(ctx: &ReducerContext, q: i32, r: i32) -> Result<(), String> {
    check_not_frozen(ctx)?;
    if geometry::in_any_island_territory(q, r) {
        return Err("cell belongs to an island".to_string());
    }
    let highest_slot = ctx.db.island().iter().map(|i| i.slot).max().unwrap_or(0);
    let bound = constants::SLOT_SPACING * (geometry::occupied_rings(highest_slot) + 1);
    if geometry::hexdist(q, r) > bound {
        return Err("cell is outside the current canvas bounds".to_string());
    }
    take_paint_token(ctx)?;
    let id = geometry::margin_cell_id(q, r);
    ctx.db.margin_cell().id().delete(id);
    Ok(())
}

/// Long-press-to-merge only ever takes from an ISLAND cell — margin tiles
/// are deliberately not a color-discovery source (design ruling: the margin
/// is a free pixel-war zone, not a place to farm unlocks; it's also
/// everyone's to paint, so a tile there gets overwritten mid-gesture far
/// more often than an island tile, which made the eyedropper feel broken in
/// practice). `cell_kind` is kept as a parameter (rather than dropped) so
/// the wire signature doesn't need to change if this is ever revisited.
#[spacetimedb::reducer]
pub fn merge_with_cell(ctx: &ReducerContext, cell_kind: u8, cell_id: u32) -> Result<(), String> {
    check_not_frozen(ctx)?;
    ctx.db.user().identity().find(ctx.sender()).ok_or("unknown user")?;
    let (tile_hue, tile_sat, tile_val, painter) = match cell_kind {
        0 => {
            let cell = ctx.db.island_cell().id().find(cell_id).ok_or("no such cell")?;
            (
                (cell.color >> 16) as u16 & 0x1FF,
                ((cell.color >> 8) & 0xFF) as u8,
                (cell.color & 0xFF) as u8,
                cell.painted_by,
            )
        }
        _ => return Err("invalid cell_kind".to_string()),
    };
    // Gate on already OWNING the color (within HUE_TOLERANCE, same window as
    // `set_brush`'s validation), not an exact match and not just on it
    // matching the current brush — a tile painted at `base - 5` while the
    // painter's brush has since drifted to `base + 5` is still "the same
    // color" as far as ownership goes, even though neither exactly equals
    // the other or the unlocked `base` entry. This also keeps the eyedropper
    // from being repeatable: after the first take, `tile_hue` is within
    // tolerance of the caller's own inventory, so a repeat long-press on the
    // same cell is rejected here instead of letting one placed tile be
    // merged against indefinitely for free XP.
    if ctx
        .db
        .inventory()
        .owner()
        .filter(&ctx.sender())
        .any(|inv| hue_dist(inv.hue, tile_hue) <= constants::HUE_TOLERANCE)
    {
        return Err("you already have this color".to_string());
    }
    // Long-press "takes" the tile's exact color rather than blending it with
    // the caller's brush (unlike cursor-merge, which does blend).
    apply_merge(ctx, ctx.sender(), painter, tile_hue, tile_sat, tile_val, false);
    Ok(())
}

#[spacetimedb::reducer]
pub fn reset_account(ctx: &ReducerContext) -> Result<(), String> {
    check_not_frozen(ctx)?;
    let user = ctx.db.user().identity().find(ctx.sender()).ok_or("unknown user")?;
    for inv in ctx.db.inventory().owner().filter(&ctx.sender()).collect::<Vec<_>>() {
        ctx.db.inventory().id().delete(inv.id);
    }
    // `start_hue` is a deterministic hash of the identity, which is fine for
    // `client_connected` (a brand-new identity is itself effectively
    // random), but reset keeps the SAME identity (decision 12) — reusing
    // `start_hue` here would silently hand the player back their exact
    // original hue every time, not a fresh roll. Use the module's actual RNG
    // instead, so repeated resets give different starting hues.
    let hue = ctx.rng().gen_range(0..360u16);
    ctx.db.inventory().insert(Inventory {
        id: 0,
        owner: ctx.sender(),
        hue,
        obtained_at: ctx.timestamp,
        obtained_with: None,
        from_gift: false,
    });
    ctx.db.user().identity().update(User {
        xp: 0,
        hue,
        sat: constants::START_SAT,
        val: constants::START_VAL,
        ..user
    });
    refresh_central_hexa(ctx);
    Ok(())
}

/// F8: like a foreign island once (XP to the owner); the (island, liker)
/// uniqueness plan.md asks for is enforced here rather than at the DB level
/// (see `IslandLike`'s comment). Self-likes are rejected — an island's own
/// "info" popup never renders a functional Like button for its owner
/// either, this is the server-side backstop for a raw reducer call.
#[spacetimedb::reducer]
pub fn like_island(ctx: &ReducerContext, island_id: u32) -> Result<(), String> {
    check_not_frozen(ctx)?;
    let island = ctx.db.island().id().find(island_id).ok_or("unknown island")?;
    if island.owner == ctx.sender() {
        return Err("cannot like your own island".to_string());
    }
    // F14 author reversal: the community island (ownerless, Identity::ZERO)
    // isn't a real player's island — no likes. Server-side backstop; both
    // clients already exclude it from the like gesture entirely.
    if island.owner == Identity::ZERO {
        return Err("cannot like the community island".to_string());
    }
    if ctx.db.island_like().island_id().filter(&island_id).any(|l| l.liker == ctx.sender()) {
        return Err("already liked".to_string());
    }
    ctx.db.island_like().insert(IslandLike { id: 0, island_id, liker: ctx.sender() });
    ctx.db.island().id().update(Island { likes: island.likes + 1, ..island.clone() });
    if let Some(owner) = ctx.db.user().identity().find(island.owner) {
        ctx.db.user().identity().update(User { xp: owner.xp + constants::XP_LIKE, ..owner });
    }
    Ok(())
}

/// F8 follow-up (author-requested): undo a like. Reverts the owner's
/// `XP_LIKE` grant too — without this, a like/unlike/like cycle would let
/// one liker re-earn the owner XP indefinitely, since the uniqueness check
/// in `like_island` only looks at the CURRENT `island_like` rows.
#[spacetimedb::reducer]
pub fn unlike_island(ctx: &ReducerContext, island_id: u32) -> Result<(), String> {
    check_not_frozen(ctx)?;
    let island = ctx.db.island().id().find(island_id).ok_or("unknown island")?;
    if island.owner == Identity::ZERO {
        return Err("cannot like the community island".to_string());
    }
    let existing = ctx
        .db
        .island_like()
        .island_id()
        .filter(&island_id)
        .find(|l| l.liker == ctx.sender())
        .ok_or("not liked")?;
    ctx.db.island_like().id().delete(existing.id);
    ctx.db.island().id().update(Island { likes: island.likes.saturating_sub(1), ..island.clone() });
    if let Some(owner) = ctx.db.user().identity().find(island.owner) {
        ctx.db.user().identity().update(User { xp: owner.xp.saturating_sub(constants::XP_LIKE), ..owner });
    }
    Ok(())
}

/// F9: set/replace the caller's own island's itch.io link — stored as just
/// the numeric submission id (decision 14); clients render the full rate URL
/// from it. Always allowed to overwrite (no confirm step needed server-side).
#[spacetimedb::reducer]
pub fn set_island_link(ctx: &ReducerContext, rate_id: u32) -> Result<(), String> {
    check_not_frozen(ctx)?;
    let island = ctx.db.island().owner().find(ctx.sender()).ok_or("you do not own an island")?;
    ctx.db.island().id().update(Island { itch_rate_id: Some(rate_id), ..island });
    Ok(())
}

/// Author-requested: pins the caller's island border to their CURRENT brush
/// color (not the seed hue the default border tracks), and un-hides it if
/// `disable_island_border` had previously hidden it.
#[spacetimedb::reducer]
pub fn set_island_border(ctx: &ReducerContext) -> Result<(), String> {
    check_not_frozen(ctx)?;
    let user = ctx.db.user().identity().find(ctx.sender()).ok_or("unknown user")?;
    let island = ctx.db.island().owner().find(ctx.sender()).ok_or("you do not own an island")?;
    let color = geometry::pack_hsv(user.hue, user.sat, user.val);
    ctx.db.island().id().update(Island { border_color: Some(color), border_hidden: false, ..island });
    Ok(())
}

/// Author-requested: makes the caller's island border transparent. Leaves
/// `border_color` untouched (rather than clearing it back to the seed-hue
/// default) so re-running `set_island_border` isn't the only way back —
/// nothing currently reads `border_color` while `border_hidden` is set.
#[spacetimedb::reducer]
pub fn disable_island_border(ctx: &ReducerContext) -> Result<(), String> {
    check_not_frozen(ctx)?;
    let island = ctx.db.island().owner().find(ctx.sender()).ok_or("you do not own an island")?;
    ctx.db.island().id().update(Island { border_hidden: true, ..island });
    Ok(())
}

/// Author-requested: the visibility toggle (Shown/Hidden) is now a separate
/// action from "set border to current color", so re-showing a previously
/// hidden border must not also clobber whatever `border_color` was set
/// before it was hidden.
#[spacetimedb::reducer]
pub fn show_island_border(ctx: &ReducerContext) -> Result<(), String> {
    check_not_frozen(ctx)?;
    let island = ctx.db.island().owner().find(ctx.sender()).ok_or("you do not own an island")?;
    ctx.db.island().id().update(Island { border_hidden: false, ..island });
    Ok(())
}

/// F10 (hexel.md's Admin section): "1 identity with a password that is
/// admin" — grants `config.admin` to whoever proves knowledge of the
/// password by hashing their input and comparing against
/// `constants::ADMIN_PASSWORD_SHA256`. Idempotent for the current admin.
/// F14 (decision 20, 2026-07-11): no longer relocates any island — slot 0 is
/// now the permanent, ownerless community island (see `paint_community_cell`
/// below), so admin is purely a role (freeze/moderation powers) with no
/// physical placement.
#[spacetimedb::reducer]
pub fn claim_admin(ctx: &ReducerContext, password: String) -> Result<(), String> {
    if hash_password(&password) != constants::ADMIN_PASSWORD_SHA256 {
        return Err("wrong password".to_string());
    }
    let config = ctx.db.config().id().find(0).ok_or("config not initialized")?;
    if config.admin == Some(ctx.sender()) {
        return Ok(());
    }
    ctx.db.user().identity().find(ctx.sender()).ok_or("unknown user")?;
    ctx.db.config().id().update(Config { admin: Some(ctx.sender()), ..config });
    Ok(())
}

/// F10 (hexel.md's Admin section): "can freeze the game so no one can
/// interact anymore" — the panic button `check_not_frozen` enforces against
/// every player-facing mutating reducer. Admin-only, and deliberately not
/// itself gated by `check_not_frozen`, so the admin can always unfreeze.
#[spacetimedb::reducer]
pub fn set_frozen(ctx: &ReducerContext, frozen: bool) -> Result<(), String> {
    require_admin(ctx)?;
    let config = ctx.db.config().id().find(0).ok_or("config not initialized")?;
    ctx.db.config().id().update(Config { frozen, ..config });
    Ok(())
}

/// Admin-only showcase/debug tool: set one uniquely named player's XP to an
/// exact value. Names are not generally unique, so reject ambiguity rather
/// than accidentally promoting the wrong player. This deliberately bypasses
/// the normal gameplay XP sources and is intended for demonstrations only.
#[spacetimedb::reducer]
pub fn admin_set_xp_by_name(ctx: &ReducerContext, name: String, xp: u64) -> Result<(), String> {
    require_admin(ctx)?;
    let matches: Vec<User> = ctx
        .db
        .user()
        .iter()
        .filter(|user| user.name.as_deref() == Some(name.as_str()))
        .collect();
    let [user] = matches.as_slice() else {
        return Err(if matches.is_empty() {
            format!("no player named {name:?}")
        } else {
            format!("multiple players are named {name:?}; choose a unique display name")
        });
    };
    let sat = user.sat.min(sat_cap(level_of(xp)));
    ctx.db.user().identity().update(User { xp, sat, ..user.clone() });
    // A promotion while the player is already waiting at the centre should
    // make them HEXA-eligible immediately, without requiring mouse motion.
    refresh_central_hexa(ctx);
    Ok(())
}

/// F10 (hexel.md's Admin section): "can delete tiles" — a moderation
/// tool for offensive/abusive island art. Wipes every painted cell on the
/// target island; the island row itself (ownership, likes, link, border,
/// slot) is untouched, so the owner keeps their spot and can repaint from
/// scratch. Admin-only, not gated by `check_not_frozen` (moderation should
/// still work while the game is frozen).
#[spacetimedb::reducer]
pub fn delete_island_cells(ctx: &ReducerContext, island_id: u32) -> Result<(), String> {
    require_admin(ctx)?;
    ctx.db.island().id().find(island_id).ok_or("unknown island")?;
    for cell in ctx.db.island_cell().island_id().filter(&island_id).collect::<Vec<_>>() {
        ctx.db.island_cell().id().delete(cell.id);
    }
    Ok(())
}

/// Mirrors the client's `island_at` (`client/src/main.rs`): which island (if
/// any) owns global cell `(q, r)`, plus its local offset. Unlike
/// `in_any_island_territory` (bool only, used by the margin reducers), the
/// admin "draw anywhere" tool needs the actual island + local coords to
/// write into `IslandCell`.
fn island_containing(ctx: &ReducerContext, q: i32, r: i32) -> Option<(Island, i32, i32)> {
    for island in ctx.db.island().iter() {
        let (sq, sr) = geometry::slot_coords(island.slot);
        let (ccx, ccy) = geometry::slot_center(sq, sr);
        let (lq, lr) = (q - ccx, r - ccy);
        if geometry::hexdist(lq, lr) <= constants::ISLAND_RADIUS {
            return Some((island, lq, lr));
        }
    }
    None
}

/// Admin-only "draw anywhere" (author request): paints whichever cell —
/// someone else's island, the community island, or the margin — contains
/// global axial `(q, r)`, bypassing the normal per-island ownership check
/// (`paint_island_cell`) and territory check (`paint_margin_cell`). Still
/// spends a paint token via `take_paint_token`, same anti-cheat/rate-limit
/// path every other paint reducer uses. Not gated by `check_not_frozen`,
/// same rationale as `delete_island_cells`: moderation/admin action, not
/// ordinary play.
#[spacetimedb::reducer]
pub fn admin_paint_cell(ctx: &ReducerContext, q: i32, r: i32) -> Result<(), String> {
    require_admin(ctx)?;
    if let Some((island, lq, lr)) = island_containing(ctx, q, r) {
        let (_, color) = take_paint_token(ctx)?;
        let id = geometry::island_cell_id(island.id, lq, lr);
        if let Some(cell) = ctx.db.island_cell().id().find(id) {
            ctx.db.island_cell().id().update(IslandCell {
                color,
                painted_by: ctx.sender(),
                painted_at: ctx.timestamp,
                ..cell
            });
        } else {
            ctx.db.island_cell().insert(IslandCell {
                id,
                island_id: island.id,
                q: lq,
                r: lr,
                color,
                painted_by: ctx.sender(),
                painted_at: ctx.timestamp,
            });
        }
        return Ok(());
    }
    let highest_slot = ctx.db.island().iter().map(|i| i.slot).max().unwrap_or(0);
    let bound = constants::SLOT_SPACING * (geometry::occupied_rings(highest_slot) + 1);
    if geometry::hexdist(q, r) > bound {
        return Err("cell is outside the current canvas bounds".to_string());
    }
    let (_, color) = take_paint_token(ctx)?;
    let id = geometry::margin_cell_id(q, r);
    if let Some(cell) = ctx.db.margin_cell().id().find(id) {
        ctx.db.margin_cell().id().update(MarginCell {
            color,
            painted_by: ctx.sender(),
            painted_at: ctx.timestamp,
            ..cell
        });
    } else {
        ctx.db.margin_cell().insert(MarginCell {
            id,
            q,
            r,
            color,
            painted_by: ctx.sender(),
            painted_at: ctx.timestamp,
        });
    }
    Ok(())
}

/// Erase counterpart to `admin_paint_cell` — same relationship
/// `erase_island_cell`/`erase_margin_cell` have to their paint counterparts.
#[spacetimedb::reducer]
pub fn admin_erase_cell(ctx: &ReducerContext, q: i32, r: i32) -> Result<(), String> {
    require_admin(ctx)?;
    if let Some((island, lq, lr)) = island_containing(ctx, q, r) {
        take_paint_token(ctx)?;
        let id = geometry::island_cell_id(island.id, lq, lr);
        ctx.db.island_cell().id().delete(id);
        return Ok(());
    }
    let highest_slot = ctx.db.island().iter().map(|i| i.slot).max().unwrap_or(0);
    let bound = constants::SLOT_SPACING * (geometry::occupied_rings(highest_slot) + 1);
    if geometry::hexdist(q, r) > bound {
        return Err("cell is outside the current canvas bounds".to_string());
    }
    take_paint_token(ctx)?;
    let id = geometry::margin_cell_id(q, r);
    ctx.db.margin_cell().id().delete(id);
    Ok(())
}

/// F9: credit an island's owner with `XP_LINK_CLICK` the first time a given
/// clicker opens its itch.io rate link; later re-opens by the same clicker
/// are free (no repeat XP) via the `island_link_click` dedupe row. Self-clicks
/// are rejected — the popup's own-island link opens the URL directly without
/// calling this reducer, so this check is the server-side backstop for a raw call.
#[spacetimedb::reducer]
pub fn click_link(ctx: &ReducerContext, island_id: u32) -> Result<(), String> {
    check_not_frozen(ctx)?;
    let island = ctx.db.island().id().find(island_id).ok_or("unknown island")?;
    if island.owner == ctx.sender() {
        return Err("cannot credit your own link click".to_string());
    }
    if island.itch_rate_id.is_none() {
        return Err("island has no link set".to_string());
    }
    if ctx.db.island_link_click().island_id().filter(&island_id).any(|c| c.clicker == ctx.sender()) {
        return Err("already credited".to_string());
    }
    ctx.db.island_link_click().insert(IslandLinkClick { id: 0, island_id, clicker: ctx.sender() });
    if let Some(owner) = ctx.db.user().identity().find(island.owner) {
        ctx.db.user().identity().update(User { xp: owner.xp + constants::XP_LINK_CLICK, ..owner });
    }
    Ok(())
}

/// F11: click/tap-to-claim. `gift_id` names the row (both clients read it
/// off the same `gift` table); the caller must be within `GIFT_CLAIM_DIST`
/// of the gift's CURRENT drifted position (`gift_drift_pos`, the same
/// formula both clients render with), checked against the caller's
/// last-reported `set_pos` cursor — same anti-cheat posture as cursor-merge.
/// Reward is a coin flip: a fresh hue (retried up to 8 times against a
/// reroll that collides with one the caller already owns, falling back to
/// `XP_GIFT` if all 8 do) or straight `XP_GIFT`.
#[spacetimedb::reducer]
pub fn claim_gift(ctx: &ReducerContext, gift_id: u64) -> Result<(), String> {
    check_not_frozen(ctx)?;
    let gift = ctx.db.gift().id().find(gift_id).ok_or("gift is gone")?;
    // A reducer returning `Err` rolls back every write it made (same as
    // every other guard in this file runs its checks before any mutation),
    // so deleting the row here would be a no-op — leave the cleanup to the
    // next `gift_tick` sweep instead.
    if ctx.timestamp >= gift.expires_at {
        return Err("gift is gone".to_string());
    }
    let user = ctx.db.user().identity().find(ctx.sender()).ok_or("unknown user")?;
    let elapsed = ctx.timestamp.duration_since(gift.spawned_at).map(|d| d.as_secs_f32()).unwrap_or(0.0).max(0.0);
    let (gx, gy) = gift_drift_pos(gift.x, gift.y, elapsed);
    let d2 = (user.cx - gx).powi(2) + (user.cy - gy).powi(2);
    if d2 > constants::GIFT_CLAIM_DIST * constants::GIFT_CLAIM_DIST {
        return Err("too far away".to_string());
    }
    ctx.db.gift().id().delete(gift.id);

    let mut rng = ctx.rng();
    if rng.gen_bool(0.5) {
        let mut granted = None;
        for _ in 0..8 {
            let candidate = rng.gen_range(0..360u16);
            let owned = ctx
                .db
                .inventory()
                .owner()
                .filter(&ctx.sender())
                .any(|inv| hue_dist(inv.hue, candidate) <= constants::HUE_TOLERANCE);
            if !owned {
                granted = Some(candidate);
                break;
            }
        }
        if let Some(hue) = granted {
            ctx.db.inventory().insert(Inventory {
                id: 0,
                owner: ctx.sender(),
                hue,
                obtained_at: ctx.timestamp,
                obtained_with: None,
                from_gift: true,
            });
            return Ok(());
        }
    }
    ctx.db.user().identity().update(User { xp: user.xp + constants::XP_GIFT, ..user });
    Ok(())
}

/// F9 time XP: grants `XP_TIME` to every user whose `last_seen` is still
/// fresh (the same presence window used for cursor visibility/merge
/// eligibility elsewhere) at each `TIME_XP_PERIOD_SECS` tick. Restricted to
/// the scheduler itself, same as the F8 re-rank reducers.
#[spacetimedb::reducer]
pub fn time_xp_tick(ctx: &ReducerContext, _arg: TimeXpSchedule) -> Result<(), String> {
    if ctx.sender() != ctx.database_identity() {
        return Err("time_xp_tick may not be invoked by clients".to_string());
    }
    for user in ctx.db.user().iter().collect::<Vec<_>>() {
        if !user.online {
            continue;
        }
        let fresh = ctx
            .timestamp
            .duration_since(user.last_seen)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(i64::MAX)
            < constants::PRESENCE_TIMEOUT_SECS;
        if fresh {
            ctx.db.user().identity().update(User { xp: user.xp + constants::XP_TIME, ..user });
        }
    }
    Ok(())
}

/// F9.5 item 10: reaps "drive-by" players — connected once, never actually
/// played, then disappeared — freeing their slot and deflating the total
/// player count. A candidate must be offline, stale
/// (`last_seen` older than `REAP_IDLE_SECS`), have an island with ZERO
/// painted cells, and an inventory of at most `REAP_MAX_INVENTORY_ROWS`
/// (just the seed hue — XP is deliberately not part of the test, since idle
/// time-XP ticks may have granted a few by the time they're stale enough).
/// Accepted edge case (author-approved): a reset veteran idling 5 minutes
/// with a still-empty island gets reaped too — the reducer can't
/// distinguish "never played" from "reset and hasn't repainted yet", and
/// jam-testing found that an acceptable tradeoff. Restricted to the
/// scheduler itself, same as every other scheduled reducer here.
#[spacetimedb::reducer]
pub fn reap_dead_players(ctx: &ReducerContext, _arg: ReapSchedule) -> Result<(), String> {
    if ctx.sender() != ctx.database_identity() {
        return Err("reap_dead_players may not be invoked by clients".to_string());
    }
    let candidates: Vec<Identity> = ctx
        .db
        .user()
        .iter()
        .filter(|u| !u.online)
        .filter(|u| {
            ctx.timestamp
                .duration_since(u.last_seen)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(i64::MAX)
                >= constants::REAP_IDLE_SECS
        })
        .map(|u| u.identity)
        .collect();

    for who in candidates {
        let Some(island) = ctx.db.island().owner().find(who) else { continue };
        if ctx.db.island_cell().iter().any(|c| c.island_id == island.id) {
            continue;
        }
        let inv_count = ctx.db.inventory().owner().filter(&who).count();
        if inv_count > constants::REAP_MAX_INVENTORY_ROWS {
            continue;
        }
        for like in ctx.db.island_like().island_id().filter(&island.id).collect::<Vec<_>>() {
            ctx.db.island_like().id().delete(like.id);
        }
        for click in ctx.db.island_link_click().island_id().filter(&island.id).collect::<Vec<_>>() {
            ctx.db.island_link_click().id().delete(click.id);
        }
        for inv in ctx.db.inventory().owner().filter(&who).collect::<Vec<_>>() {
            ctx.db.inventory().id().delete(inv.id);
        }
        ctx.db.island().id().delete(island.id);
        ctx.db.user().identity().delete(who);
    }
    Ok(())
}

/// F11: repeating tick (same lazy-seeding pattern as the other schedules) —
/// sweeps any `gift` row past its `expires_at`, then spawns a fresh one if
/// none remain. Spawn point is uniform-in-disk over the currently-occupied
/// world bound (same `occupied_rings`/`SLOT_SPACING` math
/// `paint_margin_cell` uses for its canvas bound, generously scaled up from
/// hex units to cartesian world units). Restricted to the scheduler itself,
/// same as every other scheduled reducer here.
#[spacetimedb::reducer]
pub fn gift_tick(ctx: &ReducerContext, _arg: GiftSchedule) -> Result<(), String> {
    if ctx.sender() != ctx.database_identity() {
        return Err("gift_tick may not be invoked by clients".to_string());
    }
    for g in ctx.db.gift().iter().filter(|g| g.expires_at <= ctx.timestamp).collect::<Vec<_>>() {
        ctx.db.gift().id().delete(g.id);
    }
    if ctx.db.gift().count() > 0 {
        return Ok(());
    }
    let highest_slot = ctx.db.island().iter().map(|i| i.slot).max().unwrap_or(0);
    let bound_hex = constants::SLOT_SPACING * (geometry::occupied_rings(highest_slot) + 1);
    let bound = bound_hex as f32 * 1.8;
    let mut rng = ctx.rng();
    let angle = rng.gen_range(0.0..std::f32::consts::TAU);
    let radius = bound * rng.gen_range(0.0f32..1.0).sqrt();
    let (x, y) = (radius * libm::cosf(angle), radius * libm::sinf(angle));
    ctx.db.gift().insert(Gift {
        id: 0,
        x,
        y,
        spawned_at: ctx.timestamp,
        expires_at: ctx.timestamp + TimeDuration::from_micros(constants::GIFT_LIFETIME_SECS * 1_000_000),
    });
    Ok(())
}

/// Rebuilds the fixed central formation periodically so stale occupants are
/// removed even if nobody sends another position update.
#[spacetimedb::reducer]
pub fn hexa_sweep(ctx: &ReducerContext, _arg: HexaSweepSchedule) -> Result<(), String> {
    if ctx.sender() != ctx.database_identity() {
        return Err("hexa_sweep may not be invoked by clients".to_string());
    }
    refresh_central_hexa(ctx);
    Ok(())
}

/// F8 re-rank, step 1/2 (repeating, `RERANK_PERIOD_SECS`): only sets the
/// countdown clients render, then schedules the actual sort
/// `RERANK_WARNING_SECS` later. Restricted to the scheduler itself — see the
/// SDK's documented pattern for scheduled reducers.
#[spacetimedb::reducer]
pub fn rerank_warn(ctx: &ReducerContext, _arg: RerankWarnSchedule) -> Result<(), String> {
    if ctx.sender() != ctx.database_identity() {
        return Err("rerank_warn may not be invoked by clients".to_string());
    }
    let config = ctx.db.config().id().find(0).ok_or("config missing")?;
    let fire_at = ctx.timestamp + TimeDuration::from_micros(constants::RERANK_WARNING_SECS * 1_000_000);
    ctx.db.config().id().update(Config { next_rerank_at: Some(fire_at), ..config });
    ctx.db.rerank_fire_schedule().insert(RerankFireSchedule { scheduled_id: 0, scheduled_at: fire_at.into() });
    Ok(())
}

/// F8 re-rank, step 2/2 (one-shot, fired by `rerank_warn`): re-sorts islands
/// by likes desc, then painted-tile count desc (author-requested: the
/// "hidden leaderboard" — hexel.md — should reward active painters, not
/// just liked ones), then owner name asc (author-requested), ties finally by
/// `created_at`, and rewrites `island.slot` accordingly. Slot 0 (reserved for
/// the P2/F10 admin island) is never touched — only slots 1.. are re-sorted.
/// Cell coordinates are island-relative, so moving an island is just this one
/// row write; no island_cell/margin_cell row ever needs to change.
#[spacetimedb::reducer]
pub fn rerank_fire(ctx: &ReducerContext, _arg: RerankFireSchedule) -> Result<(), String> {
    if ctx.sender() != ctx.database_identity() {
        return Err("rerank_fire may not be invoked by clients".to_string());
    }
    // One O(n) pass over every painted cell instead of a per-island scan
    // (which would be O(islands * cells)) — cheap enough to build fresh on
    // every re-rank tick (every `RERANK_PERIOD_SECS`, not per-frame).
    let mut tile_counts: HashMap<u32, u32> = HashMap::new();
    for cell in ctx.db.island_cell().iter() {
        *tile_counts.entry(cell.island_id).or_insert(0) += 1;
    }
    // Every player gets a random name at first connect (server-side, see
    // `player_label` in the client), so this is empty only in the
    // theoretical case of a still-unnamed owner.
    let names: HashMap<Identity, String> =
        ctx.db.user().iter().map(|u| (u.identity, u.name.clone().unwrap_or_default())).collect();
    let mut ranked: Vec<Island> = ctx.db.island().iter().filter(|i| i.slot != 0).collect();
    ranked.sort_by(|a, b| {
        b.likes
            .cmp(&a.likes)
            .then_with(|| tile_counts.get(&b.id).unwrap_or(&0).cmp(tile_counts.get(&a.id).unwrap_or(&0)))
            .then_with(|| names.get(&a.owner).cmp(&names.get(&b.owner)))
            .then_with(|| a.created_at.cmp(&b.created_at))
    });
    // `slot` is `#[unique]`, so a direct permutation could momentarily
    // assign a slot another still-unmoved island already holds — reslot in
    // two passes via a temporary range no real slot ever reaches.
    for island in &ranked {
        ctx.db.island().id().update(Island { slot: island.slot + 1_000_000, ..island.clone() });
    }
    for (i, island) in ranked.into_iter().enumerate() {
        ctx.db.island().id().update(Island { slot: i as u32 + 1, ..island });
    }
    if let Some(config) = ctx.db.config().id().find(0) {
        ctx.db.config().id().update(Config { next_rerank_at: None, ..config });
    }
    Ok(())
}

#[spacetimedb::reducer(client_connected)]
pub fn client_connected(ctx: &ReducerContext) {
    log::info!(
        "client connected: identity={:?} connection={:?}",
        ctx.sender(),
        ctx.connection_id()
    );
    if ctx.db.config().id().find(0).is_none() {
        ctx.db.config().insert(Config { id: 0, frozen: false, admin: None, next_rerank_at: None });
    }
    // F14 (decision 20): the community island, lazy-seeded the same way as
    // `config` — slot 0 is never assigned to a real player (`client_connected`
    // below only ever picks "next free slot >= 1"), so `Identity::ZERO` is a
    // safe, permanent sentinel meaning "no owner" without needing to touch
    // the `owner` column's type everywhere else it's read.
    if ctx.db.island().slot().find(0).is_none() {
        ctx.db.island().insert(Island {
            id: 0,
            owner: Identity::ZERO,
            slot: 0,
            likes: 0,
            itch_rate_id: None,
            created_at: ctx.timestamp,
            border_color: None,
            border_hidden: false,
        });
    }
    // Lazy-seeded the same way as the `config` row above (rather than an
    // `init` reducer) so a republish of an EXISTING database — which does
    // not re-run `init` — still ends up with the repeating re-rank timer.
    if ctx.db.rerank_warn_schedule().count() == 0 {
        ctx.db.rerank_warn_schedule().insert(RerankWarnSchedule {
            scheduled_id: 0,
            scheduled_at: TimeDuration::from_micros(constants::RERANK_PERIOD_SECS * 1_000_000).into(),
        });
    }
    // F9: same lazy-seeding pattern as the re-rank timer above.
    if ctx.db.time_xp_schedule().count() == 0 {
        ctx.db.time_xp_schedule().insert(TimeXpSchedule {
            scheduled_id: 0,
            scheduled_at: TimeDuration::from_micros(constants::TIME_XP_PERIOD_SECS * 1_000_000).into(),
        });
    }
    // F9.5 item 10: same lazy-seeding pattern as the two schedules above.
    if ctx.db.reap_schedule().count() == 0 {
        ctx.db.reap_schedule().insert(ReapSchedule {
            scheduled_id: 0,
            scheduled_at: TimeDuration::from_micros(constants::REAP_PERIOD_SECS * 1_000_000).into(),
        });
    }
    // F11: same lazy-seeding pattern as the schedules above.
    if ctx.db.gift_schedule().count() == 0 {
        ctx.db.gift_schedule().insert(GiftSchedule {
            scheduled_id: 0,
            scheduled_at: TimeDuration::from_micros(constants::GIFT_SPAWN_PERIOD_SECS * 1_000_000).into(),
        });
    }
    // F13: same lazy-seeding pattern as the schedules above.
    if ctx.db.hexa_sweep_schedule().count() == 0 {
        ctx.db.hexa_sweep_schedule().insert(HexaSweepSchedule {
            scheduled_id: 0,
            scheduled_at: TimeDuration::from_micros(constants::HEXA_SWEEP_PERIOD_SECS * 1_000_000).into(),
        });
    }

    if let Some(user) = ctx.db.user().identity().find(ctx.sender()) {
        ctx.db.user().identity().update(User { online: true, last_seen: ctx.timestamp, ..user });
        return;
    }

    let hue = start_hue(&ctx.sender());
    ctx.db.user().insert(User {
        identity: ctx.sender(),
        name: Some(random_name(ctx)),
        online: true,
        cx: 0.0,
        cy: 0.0,
        last_seen: ctx.timestamp,
        hue,
        sat: constants::START_SAT,
        val: constants::START_VAL,
        locked: false,
        xp: 0,
        paint_tokens: constants::PAINT_BUCKET_MAX,
        tokens_at: ctx.timestamp,
    });
    ctx.db.inventory().insert(Inventory {
        id: 0,
        owner: ctx.sender(),
        hue,
        obtained_at: ctx.timestamp,
        obtained_with: None,
        from_gift: false,
    });

    // Slot 0 is reserved for the (not yet implemented, P2/F10) admin island
    // at the world center — the first real player must start at slot 1.
    // F9.5 (dead-player reap): was `count() + 1`, which only ever assigned a
    // genuinely free slot while slots were never reused (no gaps possible).
    // Once reaping can delete an island out of the middle of the sequence,
    // `count()` under-counts and this would hand out a slot a still-live
    // island already owns, tripping `Island.slot`'s `#[unique]` constraint.
    // Scans for the smallest unused slot instead, which both avoids that
    // collision and reuses a reaped player's freed slot for the next
    // newcomer instead of letting the world grow unbounded.
    let slot = lowest_free_slot(ctx);
    let (q, r) = geometry::slot_coords(slot);
    let (cx, cy) = geometry::slot_center(q, r);
    let _ = (cx, cy); // fine-axial center; clients derive render position from slot (Q, R) directly
    ctx.db.island().insert(Island {
        id: 0,
        owner: ctx.sender(),
        slot,
        likes: 0,
        itch_rate_id: None,
        created_at: ctx.timestamp,
        border_color: None,
        border_hidden: false,
    });
}

#[spacetimedb::reducer(client_disconnected)]
pub fn identity_disconnected(ctx: &ReducerContext) {
    log::info!(
        "client disconnected: identity={:?} connection={:?}",
        ctx.sender(),
        ctx.connection_id()
    );
    if let Some(user) = ctx.db.user().identity().find(ctx.sender()) {
        ctx.db.user().identity().update(User { online: false, ..user });
        refresh_central_hexa(ctx);
    } else {
        log::warn!("Disconnect event for unknown user {:?}", ctx.sender());
    }
}

/// `hexa_assign_seats` is pure and can be covered without a reducer context
/// or database.
#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_3, TAU};

    fn corner_angle(slot: u32) -> f32 {
        -FRAC_PI_2 + (slot % 6) as f32 * FRAC_PI_3
    }

    #[test]
    fn fresh_six_at_corner_angles_get_matching_slots() {
        let prior = [None; 6];
        let angles: Vec<f32> = (0..6).map(corner_angle).collect();
        let seats = hexa_assign_seats(&prior, &angles);
        assert_eq!(seats, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn returning_member_keeps_its_seat() {
        // Member 0 previously sat at slot 4; a brand-new member (angle near
        // slot 1's corner) joins. Slot 4 must stay reserved for member 0
        // even though its real-world angle (near slot 0's corner here) would
        // otherwise be closer to a different free slot.
        let prior = [Some(4), None];
        let angles = [corner_angle(0), corner_angle(1)];
        let seats = hexa_assign_seats(&prior, &angles);
        assert_eq!(seats[0], 4);
        assert_eq!(seats[1], 1);
    }

    #[test]
    fn merge_collision_resolves_deterministically() {
        // Two previously-separate clusters merge; both members 0 and 1
        // claim prior seat 2 (mod 6). Identity order (index order here)
        // wins the collision: member 0 keeps slot 2, member 1 falls through
        // to pass 2 and gets reseated by nearest angle.
        let prior = [Some(2), Some(8)]; // 8 % 6 == 2, same slot as member 0
        let angles = [corner_angle(2), corner_angle(5)];
        let seats = hexa_assign_seats(&prior, &angles);
        assert_eq!(seats[0], 2);
        assert_eq!(seats[1], 5);
    }

    #[test]
    fn ang_dist_wraps_around() {
        let just_under_pi = std::f32::consts::PI - 0.1;
        let just_over_neg_pi = -std::f32::consts::PI + 0.1;
        assert!((hexa_ang_dist(just_under_pi, just_over_neg_pi) - 0.2).abs() < 1e-4);
        assert_eq!(hexa_ang_dist(0.0, TAU), 0.0);
    }
}
