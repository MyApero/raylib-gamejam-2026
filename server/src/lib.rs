use spacetimedb::rand::Rng;
use spacetimedb::{Identity, ReducerContext, ScheduleAt, Table, TimeDuration, Timestamp};

/// Canonical constants — see plan.md "Canonical constants" table. Mirror
/// these EXACTLY in the native and web clients.
mod constants {
    pub const ISLAND_RADIUS: i32 = 13;
    // Author-requested (F9.5): islands sit side by side, flat sides facing
    // flat sides (a perfect hex-of-hexes tiling — see
    // `geometry::SLOT_PLACEMENT_RADIUS`/`SLOT_U`/`SLOT_V`, which do the
    // actual placement math; a naive same-axis coarse scaling can never
    // reach zero gap at ANY spacing value, only shrink an always-triangular
    // gap toward it, because the coarse step directions point at the
    // hexagon's VERTICES rather than the middle of a flat edge), with a
    // deliberate uniform gap left between neighbors (not zero — author
    // wanted a little breathing room after all).
    //
    // MUST be even: the placement radius is bumped by `MARGIN_GAP_TILES / 2`
    // on top of `ISLAND_RADIUS` (see `geometry::SLOT_PLACEMENT_RADIUS`), and
    // that bump contributes to the gap symmetrically from both neighboring
    // islands — so the resulting gap is always exactly `2 * bump`, i.e.
    // always even. An odd value here is not geometrically reachable with
    // this tiling and would silently round down via integer division.
    pub const MARGIN_GAP_TILES: i32 = 6;
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
    pub const START_SAT: u8 = 40;
    /// F8: how often islands re-rank, and how long the client-facing
    /// countdown warns before slots actually get rewritten.
    pub const RERANK_PERIOD_SECS: i64 = 300;
    pub const RERANK_WARNING_SECS: i64 = 5;
    /// How far (degrees, either direction) a painted hue may stray from an
    /// unlocked inventory entry — lets the Hue slider nudge a shade without
    /// bloating the inventory with one row per nudge.
    pub const HUE_TOLERANCE: u16 = 5;
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

#[spacetimedb::table(accessor = config, public)]
pub struct Config {
    #[primary_key]
    id: u32,
    frozen: bool,
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

fn sat_cap(level: u64) -> u8 {
    (40 + 3 * level).min(100) as u8
}

/// Circular hue distance in degrees (handles the 359->0 wraparound).
fn hue_dist(a: u16, b: u16) -> u16 {
    let diff = (a as i32 - b as i32).unsigned_abs() as u16;
    diff.min(360 - diff)
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

#[spacetimedb::reducer]
pub fn set_pos(ctx: &ReducerContext, cx: f32, cy: f32) -> Result<(), String> {
    let user = ctx
        .db
        .user()
        .identity()
        .find(ctx.sender())
        .ok_or("unknown user")?;
    let caller = ctx.sender();
    ctx.db.user().identity().update(User {
        cx,
        cy,
        last_seen: ctx.timestamp,
        ..user
    });

    // Cursor-merge detection: nearest fresh, unlocked, differently-hued
    // online user within MERGE_DIST.
    let mut nearest: Option<(Identity, u16, f32)> = None;
    for other in ctx.db.user().iter() {
        if other.identity == caller || !other.online || other.locked {
            continue;
        }
        if ctx
            .timestamp
            .duration_since(other.last_seen)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(i64::MAX)
            >= constants::PRESENCE_TIMEOUT_SECS
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
        if !me.locked && me.hue != partner_hue {
            let merged = merge::merge_hue(me.hue, partner_hue);
            apply_merge(ctx, caller, partner, merged, true);
        }
    }
    Ok(())
}

/// Shared merge procedure for both cursor-merge and tile-merge. `cursor`
/// selects whether both sides' brush hue is updated (cursor-merge) or only
/// the caller's (tile-merge).
fn apply_merge(ctx: &ReducerContext, a: Identity, b: Identity, merged_hue: u16, cursor: bool) {
    for &who in &[a, b] {
        let already_has = ctx
            .db
            .inventory()
            .owner()
            .filter(&who)
            .any(|inv| inv.hue == merged_hue);
        if !already_has {
            let partner = if who == a { b } else { a };
            ctx.db.inventory().insert(Inventory {
                id: 0,
                owner: who,
                hue: merged_hue,
                obtained_at: ctx.timestamp,
                obtained_with: Some(partner),
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
                let sat = u.sat.min(sat_cap(level_of(u.xp)));
                ctx.db.user().identity().update(User { hue: merged_hue, sat, ..u });
            }
        }
    } else if let Some(u) = ctx.db.user().identity().find(a) {
        let sat = u.sat.min(sat_cap(level_of(u.xp)));
        ctx.db.user().identity().update(User { hue: merged_hue, sat, ..u });
    }
}

#[spacetimedb::reducer]
pub fn set_name(ctx: &ReducerContext, name: String) -> Result<(), String> {
    if name.is_empty() {
        return Err("Names must not be empty".to_string());
    }
    let user = ctx.db.user().identity().find(ctx.sender()).ok_or("unknown user")?;
    ctx.db.user().identity().update(User { name: Some(name), ..user });
    Ok(())
}

#[spacetimedb::reducer]
pub fn set_lock(ctx: &ReducerContext, locked: bool) -> Result<(), String> {
    let user = ctx.db.user().identity().find(ctx.sender()).ok_or("unknown user")?;
    ctx.db.user().identity().update(User { locked, ..user });
    Ok(())
}

#[spacetimedb::reducer]
pub fn set_brush(ctx: &ReducerContext, hue: u16, sat: u8, val: u8) -> Result<(), String> {
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

/// F9.6 item 1 (eraser, decision 18): same validation as `paint_margin_cell`
/// (canvas bound, not island territory), same token charge.
#[spacetimedb::reducer]
pub fn erase_margin_cell(ctx: &ReducerContext, q: i32, r: i32) -> Result<(), String> {
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
    ctx.db.user().identity().find(ctx.sender()).ok_or("unknown user")?;
    let (tile_hue, painter) = match cell_kind {
        0 => {
            let cell = ctx.db.island_cell().id().find(cell_id).ok_or("no such cell")?;
            ((cell.color >> 16) as u16 & 0x1FF, cell.painted_by)
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
    apply_merge(ctx, ctx.sender(), painter, tile_hue, false);
    Ok(())
}

#[spacetimedb::reducer]
pub fn reset_account(ctx: &ReducerContext) -> Result<(), String> {
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
    });
    ctx.db.user().identity().update(User {
        xp: 0,
        hue,
        sat: constants::START_SAT,
        val: 100,
        ..user
    });
    Ok(())
}

/// F8: like a foreign island once (XP to the owner); the (island, liker)
/// uniqueness plan.md asks for is enforced here rather than at the DB level
/// (see `IslandLike`'s comment). Self-likes are rejected — an island's own
/// "info" popup never renders a functional Like button for its owner
/// either, this is the server-side backstop for a raw reducer call.
#[spacetimedb::reducer]
pub fn like_island(ctx: &ReducerContext, island_id: u32) -> Result<(), String> {
    let island = ctx.db.island().id().find(island_id).ok_or("unknown island")?;
    if island.owner == ctx.sender() {
        return Err("cannot like your own island".to_string());
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
    let island = ctx.db.island().id().find(island_id).ok_or("unknown island")?;
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
    let island = ctx.db.island().owner().find(ctx.sender()).ok_or("you do not own an island")?;
    ctx.db.island().id().update(Island { itch_rate_id: Some(rate_id), ..island });
    Ok(())
}

/// F9: credit an island's owner with `XP_LINK_CLICK` the first time a given
/// clicker opens its itch.io rate link; later re-opens by the same clicker
/// are free (no repeat XP) via the `island_link_click` dedupe row. Self-clicks
/// are rejected — the popup never renders a clickable link for the owner's
/// own island either, this is the server-side backstop for a raw call.
#[spacetimedb::reducer]
pub fn click_link(ctx: &ReducerContext, island_id: u32) -> Result<(), String> {
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
/// by likes desc, ties by `created_at`, and rewrites `island.slot`
/// accordingly. Slot 0 (reserved for the P2/F10 admin island) is never
/// touched — only slots 1.. are re-sorted. Cell coordinates are
/// island-relative, so moving an island is just this one row write; no
/// island_cell/margin_cell row ever needs to change.
#[spacetimedb::reducer]
pub fn rerank_fire(ctx: &ReducerContext, _arg: RerankFireSchedule) -> Result<(), String> {
    if ctx.sender() != ctx.database_identity() {
        return Err("rerank_fire may not be invoked by clients".to_string());
    }
    let mut ranked: Vec<Island> = ctx.db.island().iter().filter(|i| i.slot != 0).collect();
    ranked.sort_by(|a, b| b.likes.cmp(&a.likes).then_with(|| a.created_at.cmp(&b.created_at)));
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
        val: 100,
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
    } else {
        log::warn!("Disconnect event for unknown user {:?}", ctx.sender());
    }
}
