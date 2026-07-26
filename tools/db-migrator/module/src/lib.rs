//! One-off migration module, published TEMPORARILY on the `hexel2` database.
//!
//! Purpose: copy the `hexel` database (a restored production backup whose
//! owner identity no longer exists locally, making it unpublishable and
//! read-only for DML) into a fresh database the local CLI identity owns,
//! while dropping the 30k+ bot-generated `merge_event` rows in flight.
//!
//! Table definitions are copied VERBATIM from `server/src/lib.rs` so the
//! game module can be republished over this one with `--delete-data=never`
//! once the copy is done (identical schema => no data-destructive changes).
//! Import order matters and is driven by the companion `migrate.py`:
//! config -> users -> islands -> advance_island_sequence -> inventory ->
//! island_cells -> margin_cells -> likes -> clicks -> hexa_events ->
//! merge_events.
//!
//! Tables whose rows must keep their exact ids (only `island`, which
//! `island_cell.island_id`/`island_like`/`island_link_click` reference)
//! are inserted with explicit ids; `advance_island_sequence` then burns
//! auto-inc values past the highest migrated id so future inserts can't
//! collide. Every other auto-inc table is renumbered fresh (`id: 0`) since
//! nothing references their ids.

use spacetimedb::{Identity, ReducerContext, ScheduleAt, Table, Timestamp};

// ---------------------------------------------------------------------------
// Table definitions — verbatim copies from server/src/lib.rs.
// ---------------------------------------------------------------------------

#[spacetimedb::table(accessor = config, public)]
pub struct Config {
    #[primary_key]
    id: u32,
    #[default(false)]
    frozen: bool,
    #[default(None::<Identity>)]
    admin: Option<Identity>,
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
    #[default(None::<u32>)]
    border_color: Option<u32>,
    #[default(false)]
    border_hidden: bool,
}

#[spacetimedb::table(accessor = island_like, public)]
pub struct IslandLike {
    #[primary_key]
    #[auto_inc]
    id: u64,
    #[index(btree)]
    island_id: u32,
    liker: Identity,
}

#[spacetimedb::table(accessor = island_link_click, public)]
pub struct IslandLinkClick {
    #[primary_key]
    #[auto_inc]
    id: u64,
    #[index(btree)]
    island_id: u32,
    clicker: Identity,
}

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

#[spacetimedb::table(accessor = time_xp_schedule, scheduled(time_xp_tick))]
pub struct TimeXpSchedule {
    #[primary_key]
    #[auto_inc]
    scheduled_id: u64,
    scheduled_at: ScheduleAt,
}

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

#[spacetimedb::table(accessor = gift_schedule, scheduled(gift_tick))]
pub struct GiftSchedule {
    #[primary_key]
    #[auto_inc]
    scheduled_id: u64,
    scheduled_at: ScheduleAt,
}

#[spacetimedb::table(accessor = hexa_reward)]
pub struct HexaReward {
    #[primary_key]
    identity: Identity,
    at: Timestamp,
}

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

#[spacetimedb::table(accessor = hexa_sweep_schedule, scheduled(hexa_sweep))]
pub struct HexaSweepSchedule {
    #[primary_key]
    #[auto_inc]
    scheduled_id: u64,
    scheduled_at: ScheduleAt,
}

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

// ---------------------------------------------------------------------------
// Scheduled-reducer stubs — names/signatures must exist for the schedule
// tables above to match the game module's schema. Bodies stay empty: the
// real implementations return when the game module is republished.
// ---------------------------------------------------------------------------

#[spacetimedb::reducer]
pub fn rerank_warn(_ctx: &ReducerContext, _arg: RerankWarnSchedule) -> Result<(), String> {
    Ok(())
}

#[spacetimedb::reducer]
pub fn rerank_fire(_ctx: &ReducerContext, _arg: RerankFireSchedule) -> Result<(), String> {
    Ok(())
}

#[spacetimedb::reducer]
pub fn time_xp_tick(_ctx: &ReducerContext, _arg: TimeXpSchedule) -> Result<(), String> {
    Ok(())
}

#[spacetimedb::reducer]
pub fn reap_dead_players(_ctx: &ReducerContext, _arg: ReapSchedule) -> Result<(), String> {
    Ok(())
}

#[spacetimedb::reducer]
pub fn gift_tick(_ctx: &ReducerContext, _arg: GiftSchedule) -> Result<(), String> {
    Ok(())
}

#[spacetimedb::reducer]
pub fn hexa_sweep(_ctx: &ReducerContext, _arg: HexaSweepSchedule) -> Result<(), String> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Import reducers — batched inserts driven by the companion migrate.py.
// ---------------------------------------------------------------------------

/// Upsert the single config row (id 0 already exists from bootstrap).
#[spacetimedb::reducer]
pub fn import_config(
    ctx: &ReducerContext,
    frozen: bool,
    admin: Option<Identity>,
    next_rerank_at: Option<Timestamp>,
) -> Result<(), String> {
    let row = Config { id: 0, frozen, admin, next_rerank_at };
    if ctx.db.config().id().find(0).is_some() {
        ctx.db.config().id().update(row);
    } else {
        ctx.db.config().insert(row);
    }
    Ok(())
}

/// The old database has no live connections; migrate every user offline.
#[spacetimedb::reducer]
pub fn import_users(ctx: &ReducerContext, users: Vec<User>) -> Result<(), String> {
    for user in users {
        ctx.db.user().insert(user);
    }
    Ok(())
}

/// Islands keep their exact ids (referenced by cells/likes/clicks). The
/// community island (id 1) already exists from bootstrap -> update it.
#[spacetimedb::reducer]
pub fn import_islands(ctx: &ReducerContext, islands: Vec<Island>) -> Result<(), String> {
    for island in islands {
        if ctx.db.island().id().find(island.id).is_some() {
            ctx.db.island().id().update(island);
        } else {
            ctx.db.island().insert(island);
        }
    }
    Ok(())
}

/// Burn `n` auto-inc island ids so the next auto-assigned id sits above the
/// highest migrated explicit id (explicit inserts don't touch the sequence).
/// Each dummy gets a distinct throwaway owner/slot: reusing one inside a
/// single transaction trips the unique index even when paired with deletes.
#[spacetimedb::reducer]
pub fn advance_island_sequence(ctx: &ReducerContext, n: u32) -> Result<(), String> {
    let mut inserted = Vec::with_capacity(n as usize);
    for i in 0..n {
        let mut bytes = [0xEEu8; 32];
        bytes[..4].copy_from_slice(&i.to_le_bytes());
        let dummy = ctx.db.island().insert(Island {
            id: 0,
            owner: Identity::from_byte_array(bytes),
            slot: u32::MAX - i,
            likes: 0,
            itch_rate_id: None,
            created_at: ctx.timestamp,
            border_color: None,
            border_hidden: false,
        });
        inserted.push(dummy.id);
    }
    for id in inserted {
        ctx.db.island().id().delete(id);
    }
    Ok(())
}

/// Inventory ids are renumbered (nothing references them); insert with
/// `id: 0` so auto-inc assigns fresh, order-preserving values.
#[spacetimedb::reducer]
pub fn import_inventory(ctx: &ReducerContext, rows: Vec<Inventory>) -> Result<(), String> {
    for row in rows {
        ctx.db.inventory().insert(Inventory { id: 0, ..row });
    }
    Ok(())
}

#[spacetimedb::reducer]
pub fn import_island_cells(ctx: &ReducerContext, rows: Vec<IslandCell>) -> Result<(), String> {
    for row in rows {
        ctx.db.island_cell().insert(row);
    }
    Ok(())
}

#[spacetimedb::reducer]
pub fn import_margin_cells(ctx: &ReducerContext, rows: Vec<MarginCell>) -> Result<(), String> {
    for row in rows {
        ctx.db.margin_cell().insert(row);
    }
    Ok(())
}

#[spacetimedb::reducer]
pub fn import_island_likes(ctx: &ReducerContext, rows: Vec<IslandLike>) -> Result<(), String> {
    for row in rows {
        ctx.db.island_like().insert(IslandLike { id: 0, ..row });
    }
    Ok(())
}

#[spacetimedb::reducer]
pub fn import_island_link_clicks(ctx: &ReducerContext, rows: Vec<IslandLinkClick>) -> Result<(), String> {
    for row in rows {
        ctx.db.island_link_click().insert(IslandLinkClick { id: 0, ..row });
    }
    Ok(())
}

#[spacetimedb::reducer]
pub fn import_hexa_events(ctx: &ReducerContext, rows: Vec<HexaEvent>) -> Result<(), String> {
    for row in rows {
        ctx.db.hexa_event().insert(HexaEvent { id: 0, ..row });
    }
    Ok(())
}

/// The whole point of the migration: only the rows that survived the bot
/// filter in migrate.py are passed in. Renumbered like the other
/// unreferenced auto-inc tables.
#[spacetimedb::reducer]
pub fn import_merge_events(ctx: &ReducerContext, rows: Vec<MergeEvent>) -> Result<(), String> {
    for row in rows {
        ctx.db.merge_event().insert(MergeEvent { id: 0, ..row });
    }
    Ok(())
}
