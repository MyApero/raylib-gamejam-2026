mod module_bindings;
mod ui;
mod world;
use module_bindings::*;
use world::constants::*;

use raylib::prelude::*;
use spacetimedb_sdk::{credentials, DbContext, Identity, Table, Timestamp};
use std::collections::HashMap;
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

fn is_present(last_seen: Timestamp, now: Timestamp) -> bool {
    now.duration_since(last_seen)
        .is_some_and(|elapsed| elapsed.as_secs() < PRESENCE_TIMEOUT_SECS as u64)
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
            ui_state.sync_name_once(user.as_ref().and_then(|u| u.name.as_ref()));

            let info = ui::HudInfo {
                me,
                level,
                xp,
                online,
                total,
                locked,
                brush: (hue, sat, val),
                sat_cap: world::sat_cap(level),
                hues: &hues,
            };
            let actions = ui::handle_input(&mut rl, &mut ui_state, &info);
            if let Some((h, s, v)) = actions.set_brush {
                let _ = ctx.reducers.set_brush(h, s, v);
                ui_state.note_used_hue(h);
            }
            if let Some(name) = actions.set_name {
                let _ = ctx.reducers.set_name(name);
            }
            if let Some(locked) = actions.set_lock {
                let _ = ctx.reducers.set_lock(locked);
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
        let map_input_allowed = !ui_state.overlay_open;

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

        // View-space culling bounds, padded well past the screen edges so
        // panning/zooming out doesn't pop islands in and out abruptly.
        let pad = (ISLAND_RADIUS as f32) * 2.0 * 5.0;
        let top_left = rl.get_screen_to_world2D(Vector2::new(0.0, 0.0), camera);
        let bottom_right = rl.get_screen_to_world2D(Vector2::new(720.0, 720.0), camera);
        let (view_min_x, view_max_x) = (top_left.x - pad, bottom_right.x + pad);
        let (view_min_y, view_max_y) = (top_left.y - pad, bottom_right.y + pad);
        let in_view = |p: Vector2| p.x >= view_min_x && p.x <= view_max_x && p.y >= view_min_y && p.y <= view_max_y;

        let island_cell_colors: HashMap<(u32, i32, i32), u32> =
            ctx.db.island_cell().iter().map(|c| ((c.island_id, c.q, c.r), c.color)).collect();

        // Screen-space projection for other players' cursors, computed here
        // (not inside the draw call) because `rl` can't be borrowed again
        // once `begin_drawing` hands out its mutable borrow below.
        let other_cursors: Vec<(Vector2, Color)> = ctx
            .db
            .user()
            .iter()
            .filter(|u| is_present(u.last_seen, now) && Some(u.identity) != me)
            .map(|u| {
                (
                    rl.get_world_to_screen2D(Vector2::new(u.cx, u.cy), camera),
                    world::hsv_color(u.hue, u.sat, u.val),
                )
            })
            .collect();

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
                for &(dq, dr) in world::island_offsets() {
                    let cell_world = world::axial_to_world(fcx + dq, fcy + dr);
                    let fill = island_cell_colors
                        .get(&(island.id, dq, dr))
                        .map_or(Color::new(60, 60, 68, 255), |&c| {
                            let (h, s, v) = world::unpack_hsv(c);
                            world::hsv_color(h, s, v)
                        });
                    world::draw_hex(&mut d2, cell_world, 1.0, fill, Color::new(40, 40, 46, 255));
                }
                if mine {
                    let r_f = ISLAND_RADIUS as f32;
                    let corners: Vec<Vector2> = world::DIRECTIONS
                        .iter()
                        .map(|&(dq, dr)| world::axial_to_world(fcx + (dq as f32 * r_f) as i32, fcy + (dr as f32 * r_f) as i32))
                        .collect();
                    for i in 0..6 {
                        d2.draw_line_ex(corners[i], corners[(i + 1) % 6], 0.3, Color::GOLD);
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
        }

        // Other players' cursors sit under the HUD (world-space indicators);
        // only the caller's own cursor needs to stay visible over the
        // header/footer/overlay, so it's drawn last, after the HUD.
        for &(screen, color) in &other_cursors {
            world::draw_cursor(&mut d, screen, color);
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
            let info = ui::HudInfo {
                me,
                level,
                xp,
                online,
                total,
                locked,
                brush,
                sat_cap: world::sat_cap(level),
                hues: &hues,
            };
            ui::draw(&mut d, &ui_state, &info);

            let (hue, sat, val) = brush;
            world::draw_cursor(&mut d, mouse_screen, world::hsv_color(hue, sat, val));
        }
        d.draw_fps(640, 10);
    }
}
