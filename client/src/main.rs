mod module_bindings;
mod sfx;
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

/// F9.6 item 7 (launch intro): a camera pose (target, zoom) framing every
/// currently-known island's center — the "whole occupied world" the intro
/// starts from before easing to the player's own island. Falls back to the
/// player-fit pose itself if no islands are known yet (can't happen once
/// `my_island` resolves, since that means at least one island — the
/// caller's own — exists; defensive only).
fn world_fit(ctx: &DbConnection, fallback: Vector2, fallback_zoom: f32) -> (Vector2, f32) {
    let mut min = Vector2::new(f32::MAX, f32::MAX);
    let mut max = Vector2::new(f32::MIN, f32::MIN);
    let mut any = false;
    for island in ctx.db.island().iter() {
        let center = island_world_center(&island);
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
    // Padding: an island's own radius (so the outermost islands aren't
    // clipped at their edge) plus a little breathing room.
    let span = (max.x - min.x).max(max.y - min.y) + ISLAND_RADIUS as f32 * 4.0;
    let zoom = (620.0 / span.max(1.0)).clamp(0.25, ISLAND_FIT_ZOOM);
    (target, zoom)
}

/// What a hovered world cell is paintable as, from `me`'s point of view.
enum Paintable {
    /// Local offset into the caller's own island.
    OwnIsland(i32, i32),
    /// Absolute world coords in the margin.
    Margin(i32, i32),
    /// F14 (decision 20): local offset into the slot-0 community island —
    /// paintable by anyone, not just its (sentinel) "owner".
    Community(i32, i32),
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
    // F14: slot 0 is always the origin (fixed geometry, never a real
    // player's island — see `lowest_free_slot`), so no island lookup needed.
    let (q0, r0) = world::slot_coords(0);
    let (ccx0, ccy0) = world::slot_center(q0, r0);
    let (lq0, lr0) = (world_q - ccx0, world_r - ccy0);
    if world::hexdist(lq0, lr0) <= ISLAND_RADIUS {
        return Paintable::Community(lq0, lr0);
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

/// F9.6 item 2 (middle-click eyedropper): the color actually painted at
/// absolute world axial `(q, r)`, whichever kind of cell it is — any
/// player's island cell, or a margin cell. `None` if nothing's been painted
/// there. Unlike `merge_target_at`, margin tiles count here (the eyedropper
/// isn't a color-discovery mechanism the way long-press-merge is — it only
/// ever picks a hue the caller already owns, see `have_hue` at the call
/// site — so the "no color discovery from the margin" rule doesn't apply).
fn painted_color_at(ctx: &DbConnection, world_q: i32, world_r: i32) -> Option<(u16, u8, u8)> {
    if let Some((island, lq, lr)) = island_at(ctx, world_q, world_r) {
        return ctx
            .db
            .island_cell()
            .iter()
            .find(|c| c.island_id == island.id && c.q == lq && c.r == lr)
            .map(|c| world::unpack_hsv(c.color));
    }
    ctx.db
        .margin_cell()
        .iter()
        .find(|c| c.q == world_q && c.r == world_r)
        .map(|c| world::unpack_hsv(c.color))
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
    // F14 (decision 20): the community island's sentinel owner isn't a real
    // player — name it "Free Isle" rather than falling through to the
    // generic "another player".
    if id == Identity::ZERO {
        return "Free Isle".to_string();
    }
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

/// Author-requested: what an island's border currently resolves to,
/// ignoring `border_hidden` — the same custom-pin-else-seed-hue fallback the
/// main draw loop's per-frame `seed_hues` branch uses (see the border
/// render code below), just computed standalone here since the popup's
/// open/refresh call sites don't have that map in scope. Feeds the
/// "Set border to current color" preview swatch.
fn resolve_border_color(ctx: &DbConnection, island: &Island) -> Color {
    if let Some(packed) = island.border_color {
        let (h, s, v) = world::unpack_hsv(packed);
        world::hsv_color(h, s, v)
    } else {
        let seed_hue = ctx.db.inventory().iter().find(|inv| inv.owner == island.owner && inv.obtained_with.is_none()).map(|inv| inv.hue);
        world::hsv_color(seed_hue.unwrap_or(0), 40, 100)
    }
}

/// F8: builds the popup payload for `island_id` and opens it. A no-op if the
/// island has since vanished (can't happen for real islands, defensive only).
fn open_island_info(ctx: &DbConnection, ui_state: &mut ui::UiState, island_id: u32, me: Identity, now: Timestamp) {
    let Some(island) = ctx.db.island().id().find(&island_id) else { return };
    let already_liked = already_liked(ctx, island_id, me);
    let border_color = resolve_border_color(ctx, &island);
    ui_state.open_island_info(ui::IslandInfo {
        island_id,
        owner_label: player_label(ctx, island.owner),
        likes: island.likes,
        age_label: format_age(now, island.created_at),
        link_id: island.itch_rate_id,
        is_own: island.owner == me,
        already_liked,
        border_color,
        border_hidden: island.border_hidden,
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
        "SELECT * FROM gift",
        "SELECT * FROM hexa_event",
        "SELECT * FROM hexa_cluster",
    ]);

    let (mut rl, thread) = raylib::init()
        .size(720, 720) // jam hard constraint
        .title("hexaworld — raylib 6.0 + SpacetimeDB")
        .build();
    rl.set_target_fps(60);
    rl.hide_cursor(); // we draw our own pointer in the caller's brush color

    // F12: audio device init is an environmental boundary (no sound card,
    // headless/CI) — degrade to silence instead of crashing the game over it.
    let audio = RaylibAudio::init_audio_device().ok();
    let sfx = audio.as_ref().map(sfx::Sfx::load);

    let mut camera = Camera2D {
        offset: Vector2::new(360.0, 360.0),
        target: Vector2::new(0.0, 0.0),
        rotation: 0.0,
        zoom: ISLAND_FIT_ZOOM,
    };
    // F9.6 item 7: `None` until the launch intro starts (the first frame
    // `my_island` resolves); `Some(start_time)` while it eases toward the
    // island; `centered_on_island` (below) becomes true once it's done or
    // skipped, same as it always has, so every OTHER frame's camera logic
    // (Center button, zoom/pan) is completely unaware the intro ever existed.
    let mut intro_started_at: Option<Instant> = None;
    let mut intro_from: Option<(Vector2, f32)> = None;
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
    // F9.6 item 2: middle-click eyedropper — press position, checked on
    // release for `MIDDLE_CLICK_TOL_PX` drift. A clean release fires the
    // eyedropper; nothing here disables the existing middle-drag PAN (see
    // `panning` below), which runs off `is_mouse_button_down` every frame
    // regardless — a real click's incidental pan delta is imperceptible.
    let mut middle_click: Option<Vector2> = None;
    // F13 (Hexa event): persisted display positions for hexagon-vertex-
    // snapped cursors (including the local player's own, per the author —
    // see `world::hexa_advance_display`'s doc comment), lerped frame to
    // frame from the server's authoritative `hexa_cluster` rows.
    let mut hexa_display: HashMap<Identity, Vector2> = HashMap::new();

    while !rl.window_should_close() {
        if let Err(e) = ctx.frame_tick() {
            eprintln!("frame_tick: {e}");
            break;
        }

        let me = ctx.try_identity();
        let now = Timestamp::now();
        let mouse_screen = rl.get_mouse_position();
        let mouse_world = rl.get_screen_to_world2D(mouse_screen, camera);

        // F11 (flying gift): current drifted world position of the single
        // active gift (if any) and whether the mouse is within claim range
        // of it right now — computed once here (same "stale by one frame"
        // convention as `mouse_world` itself: this reads last frame's
        // settled camera/mouse, same as every other click gesture below) so
        // both the claim-on-press block and the render call can reuse it.
        let active_gift: Option<(u64, Vector2, f32)> = ctx.db.gift().iter().next().map(|g| {
            let elapsed = now.duration_since(g.spawned_at).map(|d| d.as_secs_f32()).unwrap_or(0.0).max(0.0);
            let pos = world::gift_drift_pos(Vector2::new(g.x, g.y), elapsed);
            (g.id, pos, elapsed)
        });
        let gift_hit = active_gift.is_some_and(|(_, pos, _)| {
            let dx = mouse_world.x - pos.x;
            let dy = mouse_world.y - pos.y;
            (dx * dx + dy * dy).sqrt() <= GIFT_CLAIM_DIST
        });
        // The header/footer HUD bands sit ON TOP of the map, but the screen
        // coordinate underneath still maps to SOME world tile via the camera
        // transform. Without this guard, clicking a HUD button (Eraser,
        // Center, My Isle, a swatch, the name field, ...) would also
        // paint/erase/eyedrop/long-press-merge whatever tile happens to lie
        // beneath it. Gates every mouse-driven world-mutation block below;
        // camera pan/zoom are left alone since footer buttons don't handle
        // right/middle-click anyway.
        let over_map_area = mouse_screen.y > ui::HEADER_H && mouse_screen.y < (720.0 - ui::FOOTER_H);

        // F9.6 item 7: launch intro. First frame the player's own island is
        // known, camera starts framing the whole occupied world (`world_fit`)
        // and eases to the island over `INTRO_DURATION`; any input skips
        // straight to the final pose. `centered_on_island` still means
        // exactly what it always did ("the camera has settled on the
        // player's island, `main`'s other camera logic can take over") —
        // only how it gets there changed.
        if let Some(me) = me {
            if !centered_on_island {
                if let Some(island) = my_island(&ctx, me) {
                    let to_target = island_world_center(&island);
                    let to_zoom = ISLAND_FIT_ZOOM;
                    let start = *intro_started_at.get_or_insert_with(Instant::now);
                    let (from_target, from_zoom) = *intro_from.get_or_insert_with(|| world_fit(&ctx, to_target, to_zoom));
                    let any_input = rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT)
                        || rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_MIDDLE)
                        || rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_RIGHT)
                        || rl.get_mouse_wheel_move() != 0.0
                        || rl.get_key_pressed().is_some();
                    let t = (start.elapsed().as_secs_f32() / INTRO_DURATION.as_secs_f32()).clamp(0.0, 1.0);
                    if any_input || t >= 1.0 {
                        camera.target = to_target;
                        camera.zoom = to_zoom;
                        centered_on_island = true;
                    } else {
                        // Ease-in-out-cubic: slow start, fast middle, gentle
                        // landing (author-requested, other_ideas.md).
                        let ease = world::ease_in_out_cubic(t);
                        camera.target = Vector2::new(
                            from_target.x + (to_target.x - from_target.x) * ease,
                            from_target.y + (to_target.y - from_target.y) * ease,
                        );
                        camera.zoom = from_zoom + (to_zoom - from_zoom) * ease;
                    }
                    // Mouse-wheel zoom re-anchors `offset` to the cursor to
                    // zoom toward it; reset it back to screen-center or the
                    // island would land off-target after any prior scroll.
                    camera.offset = Vector2::new(360.0, 360.0);
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
                        if inv.from_gift {
                            ui_state.show_gift_toast(inv.hue);
                            if let Some(s) = &sfx {
                                s.gift.play();
                            }
                        } else if inv.obtained_with.is_none() {
                            // F13: a Hexa-pooled grant ALSO leaves
                            // `obtained_with: None` (same as a fresh seed
                            // hue or a `reset_account` reseed) — told apart
                            // by joining against a `hexa_event` row stamped
                            // with the exact same `ctx.timestamp` the server
                            // wrote both rows with in the same call (see
                            // `apply_hexa`'s comment on why this is a
                            // timestamp join rather than a new field).
                            if ctx.db.hexa_event().iter().any(|e| e.at == inv.obtained_at) {
                                ui_state.show_hexa_toast(inv.hue);
                                if let Some(s) = &sfx {
                                    s.merge.play();
                                }
                            } else {
                                ui_state.note_reset_hue(inv.hue);
                            }
                        } else {
                            let label = inv.obtained_with.map_or_else(|| "someone".to_string(), |p| player_label(&ctx, p));
                            ui_state.show_merge_toast(inv.hue, &label);
                            if let Some(s) = &sfx {
                                s.merge.play();
                            }
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
            // F9.5 item 7 follow-up: `any_modal_open` deliberately excludes
            // the foreign-island hover tooltip — it has no interactive
            // chrome to click-through onto, and blocking map input while
            // it's up would break double-click-to-like/long-press-merge on
            // the very island it's showing info for.
            suppress_map_until_release = ui_state.any_modal_open();
            // F11: a press landing on the gift claims it immediately and
            // consumes the whole gesture (like clicking through a modal's
            // close button above), so the same press can't also start a
            // paint stroke or long-press underneath it.
            if !suppress_map_until_release && over_map_area && gift_hit {
                if let Some((gift_id, _, _)) = active_gift {
                    let _ = ctx.reducers.claim_gift(gift_id);
                }
                suppress_map_until_release = true;
            }
        }
        if rl.is_mouse_button_released(MouseButton::MOUSE_BUTTON_LEFT) {
            suppress_map_until_release = false;
        }

        // HUD: snapshot server state, run widget input, apply resulting
        // reducer calls. Must run before the map-input blocks below so they
        // can see `ui_state.any_modal_open()`.
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
                if let Some(s) = &sfx {
                    s.levelup.play();
                }
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
                    let border_color = resolve_border_color(&ctx, &island);
                    ui_state.refresh_island_popup(island.likes, already_liked, island.border_hidden, border_color);
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
            if actions.set_island_border {
                let _ = ctx.reducers.set_island_border();
            }
            if actions.disable_island_border {
                let _ = ctx.reducers.disable_island_border();
            }
            if actions.show_island_border {
                let _ = ctx.reducers.show_island_border();
            }
            if let Some((island_id, rate_id)) = actions.click_link {
                open_url(&format!("https://itch.io/jam/raylib-6x-gamejam/rate/{rate_id}"));
                let _ = ctx.reducers.click_link(island_id);
            }
            if let Some(rate_id) = actions.open_own_link {
                open_url(&format!("https://itch.io/jam/raylib-6x-gamejam/rate/{rate_id}"));
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
        // F9.5 item 7 follow-up: mirrors the latch above — only an OWN-
        // island popup blocks map input; a foreign tooltip must not, or
        // hovering it would disable the very double-click/long-press
        // gestures it's showing info for.
        let map_input_allowed = !suppress_map_until_release && !ui_state.any_modal_open();

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

        // F9.6 item 6: WASD/arrow-key pan, Q/E zoom. Swallowed while a text
        // field owns keyboard input (name/import/link fields) so typing
        // doesn't also drive the camera.
        if map_input_allowed && !ui_state.text_field_focused() {
            let dt = rl.get_frame_time();
            let mut dx = 0.0;
            let mut dy = 0.0;
            if rl.is_key_down(KeyboardKey::KEY_LEFT) || rl.is_key_down(KeyboardKey::KEY_A) {
                dx -= 1.0;
            }
            if rl.is_key_down(KeyboardKey::KEY_RIGHT) || rl.is_key_down(KeyboardKey::KEY_D) {
                dx += 1.0;
            }
            if rl.is_key_down(KeyboardKey::KEY_UP) || rl.is_key_down(KeyboardKey::KEY_W) {
                dy -= 1.0;
            }
            if rl.is_key_down(KeyboardKey::KEY_DOWN) || rl.is_key_down(KeyboardKey::KEY_S) {
                dy += 1.0;
            }
            if dx != 0.0 || dy != 0.0 {
                let speed = KEY_PAN_SPEED / camera.zoom;
                camera.target.x += dx * speed * dt;
                camera.target.y += dy * speed * dt;
            }
            if rl.is_key_down(KeyboardKey::KEY_Q) {
                camera.zoom = (camera.zoom * (1.0 - KEY_ZOOM_RATE * dt)).clamp(0.25, 60.0);
            }
            if rl.is_key_down(KeyboardKey::KEY_E) {
                camera.zoom = (camera.zoom * (1.0 + KEY_ZOOM_RATE * dt)).clamp(0.25, 60.0);
            }
        }

        let panning = map_input_allowed
            && (rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_MIDDLE)
                || rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_RIGHT)
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

        // Painting/erasing: left-drag, not while panning (SHIFT/middle/right
        // held). F9.6 item 1: which reducer fires depends on
        // `ui_state.eraser_on` — same target-cell classification either way.
        // `over_map_area`: not while the cursor is over the header/footer HUD.
        if map_input_allowed && over_map_area {
            if let Some(me) = me {
                if rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT) && !panning {
                    let (wq, wr) = world::world_to_axial(mouse_world);
                    let target = match classify(&ctx, me, wq, wr) {
                        Paintable::OwnIsland(lq, lr) => Some((0u8, lq, lr)),
                        Paintable::Margin(q, r) => Some((1u8, q, r)),
                        Paintable::Community(lq, lr) => Some((2u8, lq, lr)),
                        Paintable::None => None,
                    };
                    if let Some(key) = target {
                        let fresh_cell = stroke_last != Some(key);
                        let rate_ok = last_paint_at.elapsed() >= Duration::from_secs_f32(1.0 / CLIENT_PAINT_HZ);
                        if fresh_cell && rate_ok {
                            match (key, ui_state.eraser_on) {
                                ((0, lq, lr), false) => {
                                    let _ = ctx.reducers.paint_island_cell(lq, lr);
                                }
                                ((0, lq, lr), true) => {
                                    let _ = ctx.reducers.erase_island_cell(lq, lr);
                                }
                                ((1, q, r), false) => {
                                    let _ = ctx.reducers.paint_margin_cell(q, r);
                                }
                                ((1, q, r), true) => {
                                    let _ = ctx.reducers.erase_margin_cell(q, r);
                                }
                                ((2, lq, lr), false) => {
                                    let _ = ctx.reducers.paint_community_cell(lq, lr);
                                }
                                ((2, lq, lr), true) => {
                                    let _ = ctx.reducers.erase_community_cell(lq, lr);
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

        // F9.6 item 2: middle-click eyedropper. A clean press+release within
        // `MIDDLE_CLICK_TOL_PX` picks the hovered tile's color; if the caller
        // already owns that hue (within `HUE_TOLERANCE`, same window
        // `set_brush` itself enforces), it's applied with saturation clamped
        // to the level cap — otherwise a toast explains why nothing happened.
        if !map_input_allowed {
            middle_click = None;
        } else {
            if rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_MIDDLE) && over_map_area {
                middle_click = Some(mouse_screen);
            }
            if rl.is_mouse_button_released(MouseButton::MOUSE_BUTTON_MIDDLE) {
                if let Some(press) = middle_click.take() {
                    let dx = mouse_screen.x - press.x;
                    let dy = mouse_screen.y - press.y;
                    if (dx * dx + dy * dy).sqrt() <= MIDDLE_CLICK_TOL_PX {
                        if let Some(me) = me {
                            let (wq, wr) = world::world_to_axial(mouse_world);
                            if let Some((hue, sat, val)) = painted_color_at(&ctx, wq, wr) {
                                if have_hue(&ctx, me, hue) {
                                    let level = ctx.db.user().identity().find(&me).map_or(0, |u| world::level_of(u.xp));
                                    let sat = sat.min(world::sat_cap(level));
                                    let _ = ctx.reducers.set_brush(hue, sat, val);
                                } else {
                                    ui_state.show_info_toast("not unlocked — long-press to merge".to_string());
                                    if let Some(s) = &sfx {
                                        s.error.play();
                                    }
                                }
                            }
                        }
                    }
                }
            }
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
            if !panning && over_map_area && rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT) {
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
                    // F14: the community island (owner == Identity::ZERO)
                    // opens the same popup, `player_label` renders its
                    // owner as "Free Isle" instead of a real player's name.
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
                                    && (ddx * ddx + ddy * ddy).sqrt() <= DOUBLE_CLICK_TOL_PX
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
                // F9.5 item 7 follow-up (author-requested): the hover
                // tooltip that replaced this click's old popup-opening role
                // is non-interactive, so a resolved single (non-double)
                // click on a foreign island now directly opens its itch.io
                // link (if set) instead — the same click still opens the
                // info tooltip too (touch has no hover, so this is also
                // decision 17's "tap opens" path for it).
                if let Some(island) = ctx.db.island().id().find(&island_id) {
                    if let Some(rate_id) = island.itch_rate_id {
                        open_url(&format!("https://itch.io/jam/raylib-6x-gamejam/rate/{rate_id}"));
                        let _ = ctx.reducers.click_link(island_id);
                    }
                }
                pending_info_click = None;
            }
        }

        // F9.5 item 7 / decision 17: island info on hover (desktop). Purely
        // position-based — independent of the click/long-press gesture
        // block above, which is left untouched for double-click-to-like and
        // touch (tap opens, double-tap likes; raylib-web aliases a single
        // touch to ordinary mouse events, so that path already covers touch).
        // Author-caught: clicking a footer button (e.g. "My Isle", itself
        // over foreign territory at the camera's current framing) could
        // immediately have the hover logic reinterpret that same resting
        // mouse position as hovering a DIFFERENT foreign island and
        // overwrite the just-opened own-island popup with that one —
        // `over_map_area` (computed above) guards against it.
        let currently_hovered_foreign = over_map_area
            .then(|| {
                me.and_then(|me| {
                    let (hq, hr) = world::world_to_axial(mouse_world);
                    island_at(&ctx, hq, hr)
                        .filter(|(island, _, _)| island.owner != me)
                        .map(|(island, _, _)| island.id)
                })
            })
            .flatten();
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
                // `any_modal_open` covers My Isle too — without it, hovering
                // a foreign tile would, after the delay, silently overwrite
                // or close whichever modal was open (mirrors the `is_own`
                // exemption in the hover-out close check below).
                if !ui_state.any_modal_open() && !gesturing {
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
        // island it's about should always dismiss it. Own-island popups
        // (opened via the "My Isle" footer button) are exempt: they're
        // never a hover target in the first place (`currently_hovered_foreign`
        // only ever names FOREIGN islands), so this check would otherwise
        // slam that popup shut the very next frame after opening it.
        if let Some(popup) = &ui_state.island_popup {
            if !popup.is_own && currently_hovered_foreign != Some(popup.island_id) {
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

        // F13 (Hexa event): server-authoritative cluster membership — group
        // `hexa_cluster` rows by `cluster_id`, then place each member on
        // `hexagon_vertex_positions` at ITS OWN `vertex_index` (paired
        // per-row, not by list position, so a momentarily incomplete
        // subscription snapshot can't misassign slots). `me`'s own row is
        // included here like everyone else's — per the author, the LOCAL
        // player's own cursor should visibly move to its hexagon slot too,
        // not stay glued to the mouse while everyone else's snaps (see the
        // local-cursor draw call below).
        let mut hexa_groups: HashMap<u64, Vec<HexaCluster>> = HashMap::new();
        for row in ctx.db.hexa_cluster().iter() {
            hexa_groups.entry(row.cluster_id).or_default().push(row);
        }
        let mut cluster_targets: Vec<(Vec<Identity>, Vec<Vector2>)> = Vec::new();
        // World-space hexagon edges (drawn inside `d2` below, same trick
        // `draw_gift_icon` uses). `ignited` mirrors the server's own
        // `member_count >= HEXA_SIZE` check.
        let mut hexa_polygons: Vec<(Vec<Vector2>, bool)> = Vec::new();
        for rows in hexa_groups.into_values() {
            let Some(first) = rows.first() else { continue };
            let vertices = world::hexagon_vertex_positions(Vector2::new(first.cx, first.cy), first.member_count as usize);
            let mut keys: Vec<Identity> = Vec::with_capacity(rows.len());
            let mut targets: Vec<Vector2> = Vec::with_capacity(rows.len());
            for row in &rows {
                if let Some(&v) = vertices.get(row.vertex_index as usize) {
                    keys.push(row.identity);
                    targets.push(v);
                }
            }
            hexa_polygons.push((vertices, first.ignited));
            cluster_targets.push((keys, targets));
        }
        hexa_display = world::hexa_advance_display(&hexa_display, &cluster_targets, rl.get_frame_time());

        // Screen-space projection for other players' cursors, computed here
        // (not inside the draw call) because `rl` can't be borrowed again
        // once `begin_drawing` hands out its mutable borrow below.
        let other_cursors: Vec<(Vector2, Color, bool, String)> = ctx
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
                // F13: a hexagon-cluster member renders at its lerped
                // snapped display position instead of its raw cursor
                // position — the real network position is untouched
                // (detection above keeps using it), this is purely a render
                // override.
                let world_pos = hexa_display.get(&u.identity).copied().unwrap_or(Vector2::new(u.cx, u.cy));
                (
                    rl.get_world_to_screen2D(world_pos, camera),
                    world::hsv_color(u.hue, u.sat, u.val),
                    u.locked,
                    u.name.clone().unwrap_or_default(),
                )
            })
            .collect();

        // F13 (author follow-up): the LOCAL player's own cursor also
        // renders at its lerped hexagon-snap position while participating —
        // precomputed here for the same reason `other_cursors` is (`rl`
        // can't be borrowed again once `begin_drawing` hands out its
        // mutable borrow below). `None` (not clustered, or `me` unknown
        // yet) falls back to the literal mouse position at the draw call.
        let my_hexa_screen: Option<Vector2> = me.and_then(|me| hexa_display.get(&me)).map(|&pos| rl.get_world_to_screen2D(pos, camera));

        let hover_takeable;
        let mut d = rl.begin_drawing(&thread);
        d.clear_background(Color::new(18, 18, 24, 255));

        // F9.6 item 8: past this zoom, a tile's on-screen radius is under
        // `BORDERLESS_ZOOM_THRESHOLD` px — skip the per-tile outline draw
        // call (visual noise at that size anyway; a small render win too).
        let show_tile_outline = camera.zoom >= BORDERLESS_ZOOM_THRESHOLD;

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
                // F14 (decision 20): the community island's unpainted tiles
                // are white, not the usual gray placeholder — visually marks
                // it as the shared "Free Isle" canvas at a glance.
                let unpainted_fill = if island.owner == Identity::ZERO {
                    Color::new(255, 255, 255, 255)
                } else {
                    Color::new(60, 60, 68, 255)
                };
                // F9.5 (FPS at scale): point-lookup each rendered cell by its
                // packed id via the SDK's own unique-index cache instead of
                // collecting a HashMap from EVERY island_cell row in the
                // world every frame — cost is now proportional to in-view
                // cells (this loop already skipped non-in-view islands
                // above), not total painted cells across the whole world.
                for &(dq, dr) in world::island_offsets() {
                    let cell_world = world::axial_to_world(fcx + dq, fcy + dr);
                    let id = world::island_cell_id(island.id, dq, dr);
                    let fill = ctx.db.island_cell().id().find(&id).map_or(unpainted_fill, |c| {
                        let (h, s, v) = world::unpack_hsv(c.color);
                        world::hsv_color(h, s, v)
                    });
                    world::draw_hex(&mut d2, cell_world, 1.0, fill, show_tile_outline.then_some(Color::new(40, 40, 46, 255)));
                }
                // Author-caught: sat/val used to be a fixed (85, 95),
                // making the border a different shade than the owner's
                // actual starting color. Matches `START_SAT`/100 exactly so
                // it reads as literally "their first color", not a
                // lookalike.
                //
                // Author-requested: an owner can override this default via
                // `set_island_border` (pins to their current brush color) or
                // hide it entirely via `disable_island_border` — checked
                // first since a hidden border skips the seed-hue fallback
                // too.
                let border_color = if island.border_hidden {
                    None
                } else if let Some(packed) = island.border_color {
                    let (h, s, v) = world::unpack_hsv(packed);
                    Some(world::hsv_color(h, s, v))
                } else {
                    seed_hues.get(&island.owner).map(|&hue| world::hsv_color(hue, 40, 100))
                };
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
                world::draw_hex(&mut d2, p, 1.0, world::hsv_color(h, s, v), show_tile_outline.then_some(Color::new(30, 30, 34, 255)));
            }

            // F11 (flying gift): world-space, so it naturally pans/zooms
            // with everything else.
            if let Some((_, pos, elapsed)) = active_gift {
                world::draw_gift_icon(&mut d2, pos, elapsed);
            }

            // F13 (Hexa event): world-space hexagon edges for each detected
            // cluster — see the computation above.
            for (vertices, ignited) in &hexa_polygons {
                world::draw_hexa_polygon(&mut d2, vertices, *ignited);
            }

            // Hover highlight: only on cells the caller could actually paint
            // right now (own island interior or margin) — showing it over
            // someone else's island or the inventory-overlay backdrop would
            // promise a paint that the server will reject. `over_map_area`:
            // the header/footer HUD still overlays SOME world tile per-pixel
            // (see the painting-block comment above) — without this, hovering
            // a footer button could flash the white hex through the HUD's
            // semi-transparent background.
            let (hq, hr) = world::world_to_axial(mouse_world);
            let hover_paintable =
                map_input_allowed && over_map_area && me.is_some_and(|me| !matches!(classify(&ctx, me, hq, hr), Paintable::None));
            if hover_paintable {
                let hover_center = world::axial_to_world(hq, hr);
                d2.draw_poly(hover_center, 6, 1.0, 0.0, Color::new(255, 255, 255, 70));
                // Author-requested: acts like the island border above — a
                // constant SCREEN pixel width (divided by zoom to convert
                // back to world units), not a fixed world-unit thickness, so
                // it stays visible zoomed all the way out instead of
                // shrinking under a pixel.
                d2.draw_poly_lines_ex(hover_center, 6, 1.0, 0.0, HOVER_BORDER_PX / camera.zoom, Color::new(255, 255, 255, 210));
            }

            // Eyedropper hint: a cell not paintable by the caller (someone
            // else's island — the only real long-press-merge target, see the
            // long-press block below) whose hue isn't already unlocked.
            hover_takeable = map_input_allowed
                && over_map_area
                && me.is_some_and(|me| {
                    matches!(classify(&ctx, me, hq, hr), Paintable::None)
                        && merge_target_at(&ctx, hq, hr).is_some_and(|(_, _, hue)| !have_hue(&ctx, me, hue))
                });
        }

        // Other players' cursors sit under the HUD (world-space indicators);
        // only the caller's own cursor needs to stay visible over the
        // header/footer/overlay, so it's drawn last, after the HUD.
        for (screen, color, locked, name) in &other_cursors {
            world::draw_cursor_scaled(&mut d, *screen, *color, other_cursor_scale, *locked);
            if other_cursor_scale >= 0.5 {
                world::draw_cursor_label(&mut d, *screen, name, other_cursor_scale);
            }
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
            ui::draw(&mut d, &ui_state, &info, mouse_screen);

            // F9.6 item 1: eraser mode draws the cursor in a neutral gray
            // instead of the brush hue, plus a small eraser badge — visibly
            // distinct from paint mode at a glance.
            let (hue, sat, val) = brush;
            let cursor_color = if ui_state.eraser_on { Color::new(210, 210, 216, 255) } else { world::hsv_color(hue, sat, val) };
            // F13 (author follow-up): visually snaps to the hexagon slot
            // while merging — painting/hover logic above still uses the
            // real `mouse_world`/`mouse_screen`, only this draw call moves.
            world::draw_cursor(&mut d, my_hexa_screen.unwrap_or(mouse_screen), cursor_color, locked);
        }
        if ui_state.eraser_on {
            world::draw_eraser_badge(&mut d, mouse_screen);
        } else if hover_takeable {
            world::draw_plus_hint(&mut d, mouse_screen);
        }
        if let Some(frac) = long_press.as_ref().filter(|lp| !lp.fired && lp.target.is_some()).map(|lp| {
            (lp.press_at.elapsed().as_secs_f32() / LONG_PRESS_HOLD.as_secs_f32()).clamp(0.0, 1.0)
        }) {
            world::draw_hold_ring(&mut d, mouse_screen, frac);
        }
        // Author-requested: sits in the header band (drawn here, not inside
        // `ui::draw_header`, so it's still visible before `me`/the HUD
        // itself exists) rather than floating in its own corner.
        d.draw_fps(440, 4);
    }
}
