mod module_bindings;
mod ui;
mod world;
use module_bindings::*;
use world::constants::*;

use raylib::prelude::*;
use spacetimedb_sdk::{credentials, DbContext, Identity, Table, Timestamp};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Local SpacetimeDB instance (`spacetime start`).
const HOST: &str = "http://localhost:3000";
const DB_NAME: &str = "hexmerge";
/// Client-side cap on paint-reducer calls while dragging; the server's own
/// token bucket (1000 tiles / 20 s) is the real limit, this just avoids
/// spamming calls faster than a stroke can usefully register.
const CLIENT_PAINT_HZ: f32 = 100.0;
/// Zoom level used whenever the camera centers on the player's own island
/// (startup and the footer's Center button): fits the 547-cell island
/// (radius 13, so ~22.5 world units to the furthest edge) inside the
/// 720x720 window with the header/footer bands and a little padding.
const ISLAND_FIT_ZOOM: f32 = 13.0;
/// Long-press-to-merge thresholds: hold LMB steady within `LONG_PRESS_TOL_PX`
/// screen pixels for `LONG_PRESS_HOLD` to trigger `merge_with_cell`.
const LONG_PRESS_HOLD: Duration = Duration::from_millis(400);
const LONG_PRESS_TOL_PX: f32 = 8.0;
/// Author-requested: two clean single-clicks landing on the SAME foreign
/// island within this window (and without drifting past
/// `LONG_PRESS_TOL_PX`) toggle a like/unlike instead of opening the info
/// popup — see `pending_info_click`.
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(350);
/// F9.5 item 7 / decision 17: how long the cursor must sit continuously over
/// a foreign island before its info popup opens on its own — long enough
/// that a paint stroke's cursor briefly sweeping past a neighboring border
/// doesn't flicker it open.
const HOVER_OPEN_DELAY: Duration = Duration::from_millis(200);

/// `credentials::File` keys its storage path only by this string
/// (`~/.spacetimedb_client_credentials/<key>`), shared by every process on
/// the machine — so two `cargo run` instances side by side would silently
/// load/save the SAME identity unless given distinct keys. Set
/// `HEXMERGE_PLAYER` to test as separate players locally, e.g.
/// `HEXMERGE_PLAYER=p1 cargo run -p client --bin client` in one terminal
/// and `HEXMERGE_PLAYER=p2 ...` in another. Unset defaults to the old
/// shared single-player key, so solo runs are unaffected.
fn creds_store() -> credentials::File {
    let key = std::env::var("HEXMERGE_PLAYER").unwrap_or_else(|_| DB_NAME.to_string());
    credentials::File::new(key)
}


/// The caller's own island, if the subscription has it yet.
fn my_island(ctx: &DbConnection, me: Identity) -> Option<Island> {
    ctx.db.island().owner().find(&me)
}

fn island_world_center(island: &Island) -> Vector2 {
    let (q, r) = world::slot_coords(island.slot);
    let (cx, cy) = world::slot_center(q, r);
    world::axial_to_world(cx, cy)
}

/// What a hovered world cell is paintable as, from `me`'s point of view.
enum Paintable {
    /// Local offset into the caller's own island.
    OwnIsland(i32, i32),
    /// Absolute world coords in the margin.
    Margin(i32, i32),
    /// Someone else's island, or no island yet — not paintable.
    None,
}

fn classify(ctx: &DbConnection, me: Identity, world_q: i32, world_r: i32) -> Paintable {
    if let Some(island) = my_island(ctx, me) {
        let (q, r) = world::slot_coords(island.slot);
        let (ccx, ccy) = world::slot_center(q, r);
        let (lq, lr) = (world_q - ccx, world_r - ccy);
        if world::hexdist(lq, lr) <= ISLAND_RADIUS {
            return Paintable::OwnIsland(lq, lr);
        }
    }
    if !world::in_any_island_territory(world_q, world_r) {
        return Paintable::Margin(world_q, world_r);
    }
    Paintable::None
}

/// Like `classify`, but considers every island, not just the caller's —
/// long-press merge can target any player's painted tile.
fn island_at(ctx: &DbConnection, world_q: i32, world_r: i32) -> Option<(Island, i32, i32)> {
    for island in ctx.db.island().iter() {
        let (q, r) = world::slot_coords(island.slot);
        let (ccx, ccy) = world::slot_center(q, r);
        let (lq, lr) = (world_q - ccx, world_r - ccy);
        if world::hexdist(lq, lr) <= ISLAND_RADIUS {
            return Some((island, lq, lr));
        }
    }
    None
}

/// The painted island cell (if any) at absolute world axial `(q, r)`
/// (`kind` 0, matching `merge_with_cell`'s wire signature), with its row id
/// and current hue. Snapshotted once at long-press start; only used for the
/// merge target, so staleness is harmless — the server re-checks
/// `tile_hue != me.hue` at call time and rejects a no-op merge either way.
/// Margin tiles are deliberately never a target — see the server-side
/// comment on `merge_with_cell` (no color discovery from the margin).
fn merge_target_at(ctx: &DbConnection, world_q: i32, world_r: i32) -> Option<(u8, u32, u16)> {
    let (island, lq, lr) = island_at(ctx, world_q, world_r)?;
    ctx.db
        .island_cell()
        .iter()
        .find(|c| c.island_id == island.id && c.q == lq && c.r == lr)
        .map(|c| (0u8, c.id, world::unpack_hsv(c.color).0))
}

fn short_hex(id: Identity) -> String {
    let hex = id.to_hex().to_string();
    hex[..8.min(hex.len())].to_string()
}

/// Display label for a merge partner in the "new color" toast: their name if
/// set. Every player gets a random name at first connect (server-side), so
/// this should always be set in practice; the fallback is a generic label,
/// never the partner's identity hex — a player's ID is confidential (it
/// doubles as their account-recovery token, decision 11), so it must never
/// leak to another player, not even truncated.
fn player_label(ctx: &DbConnection, id: Identity) -> String {
    ctx.db
        .user()
        .identity()
        .find(&id)
        .and_then(|u| u.name.filter(|n| !n.is_empty()))
        .unwrap_or_else(|| "another player".to_string())
}

/// Whether `me` already effectively has `hue` unlocked — long-pressing a
/// tile you already own is pointless (no new inventory row, no XP), so this
/// gates both the eyedropper itself and the "+" hover hint. Uses the same
/// `HUE_TOLERANCE` window as `set_brush`'s validation, not an exact match:
/// a tile painted at, say, `base - 5` is still "the same color" as an
/// unlocked `base` as far as ownership goes, even though the two differ by
/// a few exact degrees.
fn have_hue(ctx: &DbConnection, me: Identity, hue: u16) -> bool {
    ctx.db
        .inventory()
        .iter()
        .any(|inv| inv.owner == me && world::hue_dist(inv.hue, hue) <= HUE_TOLERANCE)
}

/// F8: coarse "N {unit} ago" label for the island-info popup — no date/time
/// crate in this workspace, and a jam popup doesn't need calendar precision.
fn format_age(now: Timestamp, created_at: Timestamp) -> String {
    let secs = now.duration_since(created_at).map(|d| d.as_secs()).unwrap_or(0);
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

/// Whether `me` has already liked `island_id` — shared by the popup payload
/// and the double-click-to-like toggle below.
fn already_liked(ctx: &DbConnection, island_id: u32, me: Identity) -> bool {
    ctx.db.island_like().iter().any(|l| l.island_id == island_id && l.liker == me)
}

/// F8: builds the popup payload for `island_id` and opens it. A no-op if the
/// island has since vanished (can't happen for real islands, defensive only).
fn open_island_info(ctx: &DbConnection, ui_state: &mut ui::UiState, island_id: u32, me: Identity, now: Timestamp) {
    let Some(island) = ctx.db.island().id().find(&island_id) else { return };
    let already_liked = already_liked(ctx, island_id, me);
    ui_state.open_island_info(ui::IslandInfo {
        island_id,
        owner_label: player_label(ctx, island.owner),
        likes: island.likes,
        age_label: format_age(now, island.created_at),
        link_id: island.itch_rate_id,
        is_own: island.owner == me,
        already_liked,
    });
}

/// In-flight long-press-to-merge gesture: started on LMB press, cancelled by
/// movement past the tolerance or button release, fires once at the hold
/// threshold. Also tracks the F8 island-info target, which fires instead on
/// a plain (short, unmoved) click — the two gestures are distinguished only
/// by hold duration, never both fire for the same press.
struct LongPress {
    press_screen: Vector2,
    press_at: Instant,
    target: Option<(u8, u32, u16)>,
    info_target: Option<u32>,
    fired: bool,
}

fn main() {
    let ctx = DbConnection::builder()
        .on_connect(|_ctx, _identity, token| {
            if let Err(e) = creds_store().save(token) {
                eprintln!("Failed to save credentials: {e:?}");
            }
        })
        .on_connect_error(|_ctx, err| {
            eprintln!("Connection error: {err:?}");
            std::process::exit(1);
        })
        .on_disconnect(|_ctx, err| {
            eprintln!("Disconnected: {err:?}");
            std::process::exit(0);
        })
        .with_token(creds_store().load().expect("Error loading credentials"))
        .with_database_name(DB_NAME)
        .with_uri(HOST)
        .build()
        .expect("Failed to connect");

    ctx.subscription_builder().on_error(|_ctx, err| {
        eprintln!("Subscription failed: {err}");
        std::process::exit(1);
    }).subscribe([
        "SELECT * FROM config",
        "SELECT * FROM user",
        "SELECT * FROM inventory",
        "SELECT * FROM island",
        "SELECT * FROM island_like",
        "SELECT * FROM island_cell",
        "SELECT * FROM margin_cell",
    ]);

    let (mut rl, thread) = raylib::init()
        .size(720, 720) // jam hard constraint
        .title("hexaworld — raylib 6.0 + SpacetimeDB")
        .build();
    rl.set_target_fps(60);
    rl.hide_cursor(); // we draw our own pointer in the caller's brush color

    let mut camera = Camera2D {
        offset: Vector2::new(360.0, 360.0),
        target: Vector2::new(0.0, 0.0),
        rotation: 0.0,
        zoom: ISLAND_FIT_ZOOM,
    };
    let mut centered_on_island = false;

    let mut last_sent_pos: Option<Vector2> = None;
    let mut last_sent_at = Instant::now();
    let mut stroke_last: Option<(u8, i32, i32)> = None;
    let mut last_paint_at = Instant::now();
    let mut ui_state = ui::UiState::new();
    let mut long_press: Option<LongPress> = None;
    // Author-requested: a clean single-click on a foreign island, held
    // pending for DOUBLE_CLICK_WINDOW to see whether a second click on the
    // same island follows (-> like/unlike toggle) before it resolves into
    // actually opening the info popup. See the gesture block below.
    let mut pending_info_click: Option<(Instant, Vector2, u32)> = None;
    // F9.5 item 7 / decision 17: `(island_id, hover_started_at)` for the
    // currently-hovered foreign island, tracked purely by cursor position
    // (independent of `long_press`/`pending_info_click` above, which stay
    // exactly as shipped for the double-click-to-like/touch-tap gesture).
    // Reset whenever the hovered island changes or the cursor leaves foreign
    // territory; only actually opens the popup once `HOVER_OPEN_DELAY` has
    // elapsed AND no paint/pan/long-press gesture is in progress.
    let mut hover_target: Option<(u32, Instant)> = None;
    // `inventory` insert-watch for the merge toast: seeded once (skipping
    // rows that already exist, e.g. the starting hue from `client_connected`)
    // so only rows inserted *after* that point are treated as "new".
    let mut known_inventory_ids: HashSet<u64> = HashSet::new();
    let mut inventory_seeded = false;
    // F9 level-up toast: `None` until the first frame `me` is known, so
    // connecting at, say, level 3 doesn't fire a spurious "level up" toast —
    // mirrors `inventory_seeded`'s seed-then-diff pattern above.
    let mut last_level: Option<u64> = None;
    // F9.5 item 5 (modal click-through): a real click's press and release
    // land on DIFFERENT frames (a mouse held for even a fraction of a second
    // spans several frames at 60fps), so gating `map_input_allowed` on only
    // the CURRENT frame's modal state isn't enough — the overlay closes on
    // the press frame, but every subsequent frame the button is still held
    // sees "no modal open" and would let the SAME press paint. Latches at
    // press-start for the whole gesture, cleared on release.
    let mut suppress_map_until_release = false;

    while !rl.window_should_close() {
        if let Err(e) = ctx.frame_tick() {
            eprintln!("frame_tick: {e}");
            break;
        }

        let me = ctx.try_identity();
        let now = Timestamp::now();
        let mouse_screen = rl.get_mouse_position();
        let mouse_world = rl.get_screen_to_world2D(mouse_screen, camera);

        if let Some(me) = me {
            if !centered_on_island {
                if let Some(island) = my_island(&ctx, me) {
                    camera.target = island_world_center(&island);
                    camera.zoom = ISLAND_FIT_ZOOM;
                    // Mouse-wheel zoom re-anchors `offset` to the cursor to
                    // zoom toward it; reset it back to screen-center or the
                    // island would land off-target after any prior scroll.
                    camera.offset = Vector2::new(360.0, 360.0);
                    centered_on_island = true;
                }
            }
        }

        // Inventory-insert watch (F4 merge feedback): toast + last3 nudge
        // when a new row for `me` appears. Runs before the HUD snapshot below
        // so a toast fired this frame is visible in this same frame's draw.
        if let Some(me) = me {
            if !inventory_seeded {
                if ctx.db.inventory().iter().any(|i| i.owner == me) {
                    known_inventory_ids = ctx.db.inventory().iter().map(|i| i.id).collect();
                    inventory_seeded = true;
                }
            } else {
                for inv in ctx.db.inventory().iter() {
                    if known_inventory_ids.insert(inv.id) && inv.owner == me {
                        // F9.5 item 4: the only way a NEW `obtained_with:
                        // None` row can appear after the initial seed above
                        // is `reset_account`'s reseed (merges always set
                        // `obtained_with: Some(partner)`) — reset the last-3
                        // ring instead of just prepending onto stale
                        // pre-reset entries.
                        if inv.obtained_with.is_none() {
                            ui_state.note_reset_hue(inv.hue);
                        } else {
                            let label = inv.obtained_with.map_or_else(|| "someone".to_string(), |p| player_label(&ctx, p));
                            ui_state.show_merge_toast(inv.hue, &label);
                        }
                    }
                }
            }
            // F9.5 item 4 (author-caught: "selected color not in recent-used
            // on launch"): seed the last-3 ring with the caller's current hue
            // the first frame it's known — a no-op once it has anything, so a
            // connection with no merge yet doesn't leave the footer empty.
            if let Some(user) = ctx.db.user().identity().find(&me) {
                ui_state.seed_last3_once(user.hue);
            }
        }

        // F9.5 item 5 (modal click-through): latch at the start of a press
        // whether a modal was open at that instant, and hold it for the
        // whole press — a click's press and release land on different
        // frames, so only the moment the overlay's close button is actually
        // hit needs checking, not every frame the button happens to still be
        // held afterward. Cleared on release so ordinary map input resumes
        // for the NEXT press.
        if rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT) {
            suppress_map_until_release =
                ui_state.overlay_open || ui_state.account_open || ui_state.island_popup.is_some();
        }
        if rl.is_mouse_button_released(MouseButton::MOUSE_BUTTON_LEFT) {
            suppress_map_until_release = false;
        }

        // HUD: snapshot server state, run widget input, apply resulting
        // reducer calls. Must run before the map-input blocks below so they
        // can see `ui_state.overlay_open` (the overlay is modal).
        let online = ctx.db.user().iter().filter(|u| u.online).count();
        let total = ctx.db.user().count() as usize;
        if let Some(me) = me {
            let user = ctx.db.user().identity().find(&me);
            let hues: Vec<u16> = ctx.db.inventory().iter().filter(|i| i.owner == me).map(|i| i.hue).collect();
            let (hue, sat, val) = user.as_ref().map_or((0, 40, 100), |u| (u.hue, u.sat, u.val));
            let xp = user.as_ref().map_or(0, |u| u.xp);
            let locked = user.as_ref().is_some_and(|u| u.locked);
            let level = world::level_of(xp);
            // F9 level-up feedback: fire once per actual increase, not on
            // the first frame `me` becomes known (that would just be
            // reporting whatever level the player already was).
            if last_level.is_some_and(|prev| level > prev) {
                ui_state.show_levelup_toast(level, world::sat_cap(level), hue);
            }
            last_level = Some(level);
            ui_state.sync_name_once(user.as_ref().and_then(|u| u.name.as_ref()));
            // Author-caught: the Like button looked unresponsive because the
            // popup's `likes`/`already_liked` were a one-time snapshot from
            // when it opened. Re-read both live every frame the popup is
            // open, same as everything else in `HudInfo`.
            if let Some(island_id) = ui_state.island_popup.as_ref().map(|p| p.island_id) {
                if let Some(island) = ctx.db.island().id().find(&island_id) {
                    let already_liked = ctx.db.island_like().iter().any(|l| l.island_id == island_id && l.liker == me);
                    ui_state.refresh_island_popup(island.likes, already_liked);
                }
            }
            // F8 re-rank countdown: `next_rerank_at.duration_since(now)` is
            // `Some` only while the target is still in the future.
            let rerank_secs = ctx
                .db
                .config()
                .id()
                .find(&0)
                .and_then(|c| c.next_rerank_at)
                .and_then(|t| t.duration_since(now))
                .map(|d| d.as_secs_f32().ceil() as i64);

            let short_id = short_hex(me);
            let info = ui::HudInfo {
                short_id: &short_id,
                level,
                xp,
                online,
                total,
                locked,
                brush: (hue, sat, val),
                sat_cap: world::sat_cap(level),
                hues: &hues,
                show_token_import: false,
                rerank_secs,
            };
            let actions = ui::handle_input(&mut rl, &mut ui_state, &info);
            // Note: last-3 tracking happens inside `ui::handle_input` itself
            // (only on an explicit swatch/last-3 click), NOT here — every
            // `set_brush` action also fires continuously while dragging the
            // Hue/Sat/Val sliders, which must not spam the last-3 ring.
            if let Some((h, s, v)) = actions.set_brush {
                let _ = ctx.reducers.set_brush(h, s, v);
            }
            if let Some(name) = actions.set_name {
                let _ = ctx.reducers.set_name(name);
            }
            if let Some(locked) = actions.set_lock {
                let _ = ctx.reducers.set_lock(locked);
            }
            if actions.copy_token {
                match creds_store().load() {
                    Ok(Some(token)) => {
                        if let Err(e) = rl.set_clipboard_text(&token) {
                            eprintln!("Failed to copy token to clipboard: {e:?}");
                        }
                    }
                    Ok(None) => eprintln!("No saved credentials to copy yet"),
                    Err(e) => eprintln!("Failed to load credentials: {e:?}"),
                }
            }
            // Native token import is skipped for the jam (plan.md F6): the
            // judged target is the web build, where importing reconnects via
            // a page reload; native has no equivalent hot-swap short of
            // restarting the process. `show_token_import: false` above means
            // `ui::handle_input` never actually produces this action here.
            let _ = actions.import_token;
            if actions.reset_account {
                let _ = ctx.reducers.reset_account();
            }
            if let Some(island_id) = actions.like_island {
                let _ = ctx.reducers.like_island(island_id);
            }
            if let Some(island_id) = actions.unlike_island {
                let _ = ctx.reducers.unlike_island(island_id);
            }
            if let Some(rate_id) = actions.set_island_link {
                let _ = ctx.reducers.set_island_link(rate_id);
            }
            if let Some((island_id, rate_id)) = actions.click_link {
                open_url(&format!("https://itch.io/jam/raylib-6x-gamejam/rate/{rate_id}"));
                let _ = ctx.reducers.click_link(island_id);
            }
            // Author-requested: footer button replacing the old "click your
            // own island" gesture, which just painted instead of opening
            // the popup.
            if actions.open_own_island {
                if let Some(island) = my_island(&ctx, me) {
                    open_island_info(&ctx, &mut ui_state, island.id, me, now);
                }
            }
            if actions.center_camera {
                if let Some(island) = my_island(&ctx, me) {
                    camera.target = island_world_center(&island);
                    camera.zoom = ISLAND_FIT_ZOOM;
                    // Mouse-wheel zoom re-anchors `offset` to the cursor to
                    // zoom toward it; reset it back to screen-center or the
                    // island would land off-target after any prior scroll.
                    camera.offset = Vector2::new(360.0, 360.0);
                }
            }
        }
        let map_input_allowed = !suppress_map_until_release
            && !ui_state.overlay_open
            && !ui_state.account_open
            && ui_state.island_popup.is_none();

        // Zoom toward the cursor (official raylib recipe): re-anchor
        // offset/target at the mouse before changing zoom so the world
        // point under the cursor doesn't jump.
        let wheel = if map_input_allowed { rl.get_mouse_wheel_move() } else { 0.0 };
        if wheel != 0.0 {
            camera.offset = mouse_screen;
            camera.target = mouse_world;
            // Max zoom: ~7 tiles (hex center-to-center spacing is sqrt(3)
            // world units) should be able to fill the 720px window.
            camera.zoom = (camera.zoom * (1.0 + wheel * 0.1)).clamp(0.25, 60.0);
        }

        let panning = map_input_allowed
            && (rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_MIDDLE)
                || (rl.is_key_down(KeyboardKey::KEY_LEFT_SHIFT)
                    && rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT)));
        if panning {
            let delta = rl.get_mouse_delta();
            camera.target.x -= delta.x / camera.zoom;
            camera.target.y -= delta.y / camera.zoom;
        }

        // Cursor heartbeat: throttled to CURSOR_SEND_HZ and only when moved.
        if map_input_allowed && me.is_some() {
            let moved = last_sent_pos.is_none_or(|p| (p.x - mouse_world.x).abs() > 1e-4 || (p.y - mouse_world.y).abs() > 1e-4);
            if moved && last_sent_at.elapsed() >= Duration::from_secs_f32(1.0 / CURSOR_SEND_HZ) {
                let _ = ctx.reducers.set_pos(mouse_world.x, mouse_world.y);
                last_sent_pos = Some(mouse_world);
                last_sent_at = Instant::now();
            }
        }

        // Painting: left-drag, not while panning (SHIFT held).
        if map_input_allowed {
            if let Some(me) = me {
                if rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT) && !panning {
                    let (wq, wr) = world::world_to_axial(mouse_world);
                    let target = match classify(&ctx, me, wq, wr) {
                        Paintable::OwnIsland(lq, lr) => Some((0u8, lq, lr)),
                        Paintable::Margin(q, r) => Some((1u8, q, r)),
                        Paintable::None => None,
                    };
                    if let Some(key) = target {
                        let fresh_cell = stroke_last != Some(key);
                        let rate_ok = last_paint_at.elapsed() >= Duration::from_secs_f32(1.0 / CLIENT_PAINT_HZ);
                        if fresh_cell && rate_ok {
                            match key {
                                (0, lq, lr) => {
                                    let _ = ctx.reducers.paint_island_cell(lq, lr);
                                }
                                (1, q, r) => {
                                    let _ = ctx.reducers.paint_margin_cell(q, r);
                                }
                                _ => unreachable!(),
                            }
                            stroke_last = Some(key);
                            last_paint_at = Instant::now();
                        }
                    }
                }
            }
        }
        if rl.is_mouse_button_released(MouseButton::MOUSE_BUTTON_LEFT) {
            stroke_last = None;
        }

        // Long-press-to-merge: hold LMB steady on a painted cell for
        // LONG_PRESS_HOLD -> merge_with_cell. Doubles as "a cell you're NOT
        // painting": if the cell was paintable by you, the ordinary
        // paint-on-press call above already overwrote it with your own hue,
        // so by the time this timer would fire the tile no longer differs
        // from your brush and the server rejects the merge as a no-op.
        // `target` is filtered to hues not already in the caller's
        // inventory at press-time — long-pressing a color you already own
        // gets no hold ring at all, matching the "+" hover hint below.
        if !map_input_allowed {
            long_press = None;
        } else if let Some(me) = me {
            if !panning && rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT) {
                let (wq, wr) = world::world_to_axial(mouse_world);
                long_press = Some(LongPress {
                    press_screen: mouse_screen,
                    press_at: Instant::now(),
                    target: merge_target_at(&ctx, wq, wr).filter(|&(_, _, hue)| !have_hue(&ctx, me, hue)),
                    // F8 (author follow-up): any point on a FOREIGN island's
                    // territory, not just its center — released here opens
                    // its info popup instead of painting. Your own island
                    // stays paint-only; its popup now opens via the "My
                    // Isle" footer button instead (`actions.open_own_island`).
                    info_target: island_at(&ctx, wq, wr)
                        .filter(|(island, _, _)| island.owner != me)
                        .map(|(island, _, _)| island.id),
                    fired: false,
                });
            }
            if let Some(lp) = &mut long_press {
                let dx = mouse_screen.x - lp.press_screen.x;
                let dy = mouse_screen.y - lp.press_screen.y;
                let moved = (dx * dx + dy * dy).sqrt() > LONG_PRESS_TOL_PX;
                let released = !rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT);
                if moved || released {
                    // Author-requested (Instagram-style): a clean short
                    // click on a foreign island no longer opens the popup
                    // immediately — it's held as `pending_info_click` for
                    // DOUBLE_CLICK_WINDOW first, in case a second click
                    // lands on the same island within it, which instead
                    // toggles a like/unlike (with a floating +1/-1 pop) and
                    // cancels the popup-open entirely. If no second click
                    // arrives, the deferred-resolution check below opens the
                    // popup once the window elapses. The trade-off is a
                    // short, deliberate delay before a single click's popup
                    // appears — otherwise the popup (which blocks further
                    // map input while open) would swallow the second click
                    // of every double-click before it could ever register.
                    if released && !moved && !lp.fired {
                        if let Some(island_id) = lp.info_target {
                            let is_double = pending_info_click.is_some_and(|(t, pos, id)| {
                                let ddx = pos.x - mouse_screen.x;
                                let ddy = pos.y - mouse_screen.y;
                                id == island_id
                                    && t.elapsed() < DOUBLE_CLICK_WINDOW
                                    && (ddx * ddx + ddy * ddy).sqrt() <= LONG_PRESS_TOL_PX
                            });
                            if is_double {
                                let liked = already_liked(&ctx, island_id, me);
                                if liked {
                                    let _ = ctx.reducers.unlike_island(island_id);
                                } else {
                                    let _ = ctx.reducers.like_island(island_id);
                                }
                                ui_state.spawn_like_anim(mouse_screen, !liked);
                                pending_info_click = None;
                            } else {
                                pending_info_click = Some((Instant::now(), mouse_screen, island_id));
                            }
                        }
                    }
                    long_press = None;
                } else if !lp.fired && lp.press_at.elapsed() >= LONG_PRESS_HOLD {
                    lp.fired = true;
                    if let Some((kind, id, _)) = lp.target {
                        let _ = ctx.reducers.merge_with_cell(kind, id);
                    }
                }
            }
        } else {
            long_press = None;
        }

        // Resolves a `pending_info_click` into an actual popup-open once
        // DOUBLE_CLICK_WINDOW has passed without a follow-up click landing
        // on the same island (which would have consumed it as a
        // like/unlike toggle instead, above). Runs every frame,
        // independently of this frame's press/release state.
        if let Some((clicked_at, _, island_id)) = pending_info_click {
            if clicked_at.elapsed() >= DOUBLE_CLICK_WINDOW {
                if let Some(me) = me {
                    open_island_info(&ctx, &mut ui_state, island_id, me, now);
                }
                pending_info_click = None;
            }
        }

        // F9.5 item 7 / decision 17: island info on hover (desktop). Purely
        // position-based — independent of the click/long-press gesture
        // block above, which is left untouched for double-click-to-like and
        // touch (tap opens, double-tap likes; raylib-web aliases a single
        // touch to ordinary mouse events, so that path already covers touch).
        let currently_hovered_foreign = me.and_then(|me| {
            let (hq, hr) = world::world_to_axial(mouse_world);
            island_at(&ctx, hq, hr).filter(|(island, _, _)| island.owner != me).map(|(island, _, _)| island.id)
        });
        match currently_hovered_foreign {
            Some(id) => {
                if hover_target.map(|(hid, _)| hid) != Some(id) {
                    hover_target = Some((id, Instant::now()));
                }
                // Suppressed while any button is held (mid paint stroke,
                // pan, or long-press) so a drag sweeping past a neighboring
                // island's border doesn't flicker its popup open.
                let gesturing = rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT)
                    || rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_MIDDLE);
                if !ui_state.overlay_open && !ui_state.account_open && !gesturing {
                    if let Some((hid, since)) = hover_target {
                        let already_open = ui_state.island_popup.as_ref().is_some_and(|p| p.island_id == id);
                        if hid == id && !already_open && since.elapsed() >= HOVER_OPEN_DELAY {
                            if let Some(me) = me {
                                open_island_info(&ctx, &mut ui_state, id, me, now);
                            }
                        }
                    }
                }
            }
            None => hover_target = None,
        }
        // Closes on hover-out, regardless of how the popup was opened
        // (hover or the click/double-click path above) — moving off the
        // island it's about should always dismiss it.
        if let Some(popup) = &ui_state.island_popup {
            if currently_hovered_foreign != Some(popup.island_id) {
                ui_state.island_popup = None;
            }
        }

        // View-space culling bounds, padded well past the screen edges so
        // panning/zooming out doesn't pop islands in and out abruptly.
        let pad = (ISLAND_RADIUS as f32) * 2.0 * 5.0;
        let top_left = rl.get_screen_to_world2D(Vector2::new(0.0, 0.0), camera);
        let bottom_right = rl.get_screen_to_world2D(Vector2::new(720.0, 720.0), camera);
        let (view_min_x, view_max_x) = (top_left.x - pad, bottom_right.x + pad);
        let (view_min_y, view_max_y) = (top_left.y - pad, bottom_right.y + pad);
        let in_view = |p: Vector2| p.x >= view_min_x && p.x <= view_max_x && p.y >= view_min_y && p.y <= view_max_y;

        // Author-requested (F8 follow-up): each island's border is drawn in
        // its owner's SEED hue — the color they started with (or re-rolled
        // via reset), never the live/nudged brush — so the world map is
        // browsable by "whose island is that" at a glance. Exactly one
        // `obtained_with.is_none()` row exists per owner at any time (the
        // original `client_connected` seed, or `reset_account`'s reseed,
        // which deletes every prior row first).
        let seed_hues: HashMap<Identity, u16> = ctx
            .db
            .inventory()
            .iter()
            .filter(|inv| inv.obtained_with.is_none())
            .map(|inv| (inv.owner, inv.hue))
            .collect();

        // F9.5 item 6: render scale for other players' cursors, relative to
        // their fixed on-screen size at the default `ISLAND_FIT_ZOOM` —
        // shrinks with the camera like a world-space object would (instead
        // of towering over the tiles when zoomed out), floored so it stays
        // findable even zoomed far out.
        let other_cursor_scale = (camera.zoom / ISLAND_FIT_ZOOM).max(world::constants::CURSOR_MIN_SCALE);

        // Screen-space projection for other players' cursors, computed here
        // (not inside the draw call) because `rl` can't be borrowed again
        // once `begin_drawing` hands out its mutable borrow below.
        let other_cursors: Vec<(Vector2, Color)> = ctx
            .db
            .user()
            .iter()
            // Author-reported: cursors used to vanish ~3s after a player
            // stopped moving their mouse (`is_present`'s freshness window,
            // meant for merge-eligibility, was also gating cursor
            // rendering) — a stationary-but-connected player should stay
            // visible the whole time they're online, not just while
            // actively moving.
            .filter(|u| u.online && Some(u.identity) != me)
            .map(|u| {
                (
                    rl.get_world_to_screen2D(Vector2::new(u.cx, u.cy), camera),
                    world::hsv_color(u.hue, u.sat, u.val),
                )
            })
            .collect();

        let hover_takeable;
        let mut d = rl.begin_drawing(&thread);
        d.clear_background(Color::new(18, 18, 24, 255));

        {
            let mut d2 = d.begin_mode2D(camera);

            for island in ctx.db.island().iter() {
                let (q, r) = world::slot_coords(island.slot);
                let (fcx, fcy) = world::slot_center(q, r);
                let center = world::axial_to_world(fcx, fcy);
                if !in_view(center) {
                    continue;
                }
                let mine = me == Some(island.owner);
                // F9.5 (FPS at scale): point-lookup each rendered cell by its
                // packed id via the SDK's own unique-index cache instead of
                // collecting a HashMap from EVERY island_cell row in the
                // world every frame — cost is now proportional to in-view
                // cells (this loop already skipped non-in-view islands
                // above), not total painted cells across the whole world.
                for &(dq, dr) in world::island_offsets() {
                    let cell_world = world::axial_to_world(fcx + dq, fcy + dr);
                    let id = world::island_cell_id(island.id, dq, dr);
                    let fill = ctx.db.island_cell().id().find(&id).map_or(Color::new(60, 60, 68, 255), |c| {
                        let (h, s, v) = world::unpack_hsv(c.color);
                        world::hsv_color(h, s, v)
                    });
                    world::draw_hex(&mut d2, cell_world, 1.0, fill, Color::new(40, 40, 46, 255));
                }
                // Author-caught: sat/val used to be a fixed (85, 95),
                // making the border a different shade than the owner's
                // actual starting color. Matches `START_SAT`/100 exactly so
                // it reads as literally "their first color", not a
                // lookalike.
                let border_color = seed_hues.get(&island.owner).map(|&hue| world::hsv_color(hue, 40, 100));
                if let Some(border_color) = border_color {
                    let r_f = ISLAND_RADIUS as f32;
                    let corners: Vec<Vector2> = world::DIRECTIONS
                        .iter()
                        .map(|&(dq, dr)| world::axial_to_world(fcx + (dq as f32 * r_f) as i32, fcy + (dr as f32 * r_f) as i32))
                        .collect();
                    // Own island's border is drawn thicker — still colored
                    // by identity like every other island, just easier to
                    // pick out as "mine" at a glance.
                    //
                    // Author-caught: thickness used to be a fixed WORLD-unit
                    // value, so `BeginMode2D`'s zoom scaled it down along
                    // with everything else — at low zoom it fell under a
                    // screen pixel, and different edges (each at a slightly
                    // different angle) rounded to zero at slightly different
                    // zoom levels, making one side of the hexagon vanish
                    // before the others. Converting the desired SCREEN pixel
                    // width back to world units (dividing by zoom) keeps the
                    // rendered line a constant, always-visible thickness
                    // regardless of zoom.
                    let px = if mine { 3.0 } else { 1.5 };
                    let thickness = px / camera.zoom;
                    for i in 0..6 {
                        d2.draw_line_ex(corners[i], corners[(i + 1) % 6], thickness, border_color);
                    }
                }
            }

            for cell in ctx.db.margin_cell().iter() {
                let p = world::axial_to_world(cell.q, cell.r);
                if !in_view(p) {
                    continue;
                }
                let (h, s, v) = world::unpack_hsv(cell.color);
                world::draw_hex(&mut d2, p, 1.0, world::hsv_color(h, s, v), Color::new(30, 30, 34, 255));
            }

            // Hover highlight: only on cells the caller could actually paint
            // right now (own island interior or margin) — showing it over
            // someone else's island or the inventory-overlay backdrop would
            // promise a paint that the server will reject.
            let (hq, hr) = world::world_to_axial(mouse_world);
            let hover_paintable =
                map_input_allowed && me.is_some_and(|me| !matches!(classify(&ctx, me, hq, hr), Paintable::None));
            if hover_paintable {
                let hover_center = world::axial_to_world(hq, hr);
                d2.draw_poly(hover_center, 6, 1.0, 0.0, Color::new(255, 255, 255, 70));
                d2.draw_poly_lines_ex(hover_center, 6, 1.0, 0.0, 0.06, Color::new(255, 255, 255, 210));
            }

            // Eyedropper hint: a cell not paintable by the caller (someone
            // else's island — the only real long-press-merge target, see the
            // long-press block below) whose hue isn't already unlocked.
            hover_takeable = map_input_allowed
                && me.is_some_and(|me| {
                    matches!(classify(&ctx, me, hq, hr), Paintable::None)
                        && merge_target_at(&ctx, hq, hr).is_some_and(|(_, _, hue)| !have_hue(&ctx, me, hue))
                });
        }

        // Other players' cursors sit under the HUD (world-space indicators);
        // only the caller's own cursor needs to stay visible over the
        // header/footer/overlay, so it's drawn last, after the HUD.
        for &(screen, color) in &other_cursors {
            world::draw_cursor_scaled(&mut d, screen, color, other_cursor_scale);
        }

        let own_brush = me.map(|me| {
            let user = ctx.db.user().identity().find(&me);
            (
                user.as_ref().map_or((0, 40, 100), |u| (u.hue, u.sat, u.val)),
                user.as_ref().map_or(0, |u| u.xp),
                user.as_ref().is_some_and(|u| u.locked),
            )
        });
        if let (Some(me), Some((brush, xp, locked))) = (me, own_brush) {
            let hues: Vec<u16> = ctx.db.inventory().iter().filter(|i| i.owner == me).map(|i| i.hue).collect();
            let level = world::level_of(xp);
            let short_id = short_hex(me);
            let rerank_secs = ctx
                .db
                .config()
                .id()
                .find(&0)
                .and_then(|c| c.next_rerank_at)
                .and_then(|t| t.duration_since(now))
                .map(|d| d.as_secs_f32().ceil() as i64);
            let info = ui::HudInfo {
                short_id: &short_id,
                level,
                xp,
                online,
                total,
                locked,
                brush,
                sat_cap: world::sat_cap(level),
                hues: &hues,
                show_token_import: false,
                rerank_secs,
            };
            ui::draw(&mut d, &ui_state, &info);

            let (hue, sat, val) = brush;
            world::draw_cursor(&mut d, mouse_screen, world::hsv_color(hue, sat, val));
        }
        if hover_takeable {
            world::draw_plus_hint(&mut d, mouse_screen);
        }
        if let Some(frac) = long_press.as_ref().filter(|lp| !lp.fired && lp.target.is_some()).map(|lp| {
            (lp.press_at.elapsed().as_secs_f32() / LONG_PRESS_HOLD.as_secs_f32()).clamp(0.0, 1.0)
        }) {
            world::draw_hold_ring(&mut d, mouse_screen, frac);
        }
        d.draw_fps(640, 10);
    }
}
