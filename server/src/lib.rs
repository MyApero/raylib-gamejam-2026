use spacetimedb::{Identity, ReducerContext, Table, Timestamp};

/// Canonical constants — see plan.md "Canonical constants" table. Mirror
/// these EXACTLY in the native and web clients.
mod constants {
    pub const ISLAND_RADIUS: i32 = 13;
    pub const SLOT_SPACING: i32 = 29;
    pub const MERGE_DIST: f32 = 1.0;
    // 50x the original plan.md values (1000 tiles / 20s sustained) — author
    // felt the original paint rate limit too restrictive in hand-testing.
    pub const PAINT_BUCKET_MAX: f32 = 1000.0;
    pub const PAINT_REFILL_PER_SEC: f32 = 50.0;
    pub const PRESENCE_TIMEOUT_SECS: i64 = 3;
    pub const XP_MERGE_NEW: u64 = 25;
    pub const LEVEL_XP: u64 = 100;
    pub const START_SAT: u8 = 40;
    /// How far (degrees, either direction) a painted hue may stray from an
    /// unlocked inventory entry — lets the Hue slider nudge a shade without
    /// bloating the inventory with one row per nudge.
    pub const HUE_TOLERANCE: u16 = 5;
}

/// Axial hex geometry, slot-lattice mapping and cell-id packing — see
/// plan.md "Geometry spec". Pure functions only; no DB access.
mod geometry {
    use super::constants::{ISLAND_RADIUS, SLOT_SPACING};

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

    /// Fine axial center of a coarse slot.
    pub fn slot_center(q: i32, r: i32) -> (i32, i32) {
        (SLOT_SPACING * q, SLOT_SPACING * r)
    }

    /// True if the fine world cell `(q, r)` belongs to some island's
    /// interior (occupied or not — the whole coarse lattice is reserved).
    pub fn in_any_island_territory(q: i32, r: i32) -> bool {
        let (cq, cr) = cube_round(q as f32 / SLOT_SPACING as f32, r as f32 / SLOT_SPACING as f32);
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
    // Player islands occupy slots 1..=count (slot 0 is reserved for admin).
    let highest_slot = ctx.db.island().count() as u32;
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
    let hue = start_hue(&ctx.sender());
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

#[spacetimedb::reducer(client_connected)]
pub fn client_connected(ctx: &ReducerContext) {
    log::info!(
        "client connected: identity={:?} connection={:?}",
        ctx.sender(),
        ctx.connection_id()
    );
    if ctx.db.config().id().find(0).is_none() {
        ctx.db.config().insert(Config { id: 0, frozen: false, admin: None });
    }

    if let Some(user) = ctx.db.user().identity().find(ctx.sender()) {
        ctx.db.user().identity().update(User { online: true, last_seen: ctx.timestamp, ..user });
        return;
    }

    let hue = start_hue(&ctx.sender());
    ctx.db.user().insert(User {
        identity: ctx.sender(),
        name: None,
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
    let slot = ctx.db.island().count() as u32 + 1;
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
