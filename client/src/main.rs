mod hexgrid;
mod module_bindings;
use hexgrid::*;
use module_bindings::*;

use raylib::prelude::*;
use spacetimedb_sdk::{credentials, DbContext, Table, Timestamp};
use std::collections::HashMap;

/// Local SpacetimeDB instance (`spacetime start`).
const HOST: &str = "http://localhost:3000";
const DB_NAME: &str = "hexmerge";
/// A user's cursor is drawn as long as they've sent a position update within
/// this window. Web clients poll instead of holding a live connection, so
/// presence is judged by recency rather than the `online` flag (which HTTP
/// polling would flap on every request).
const PRESENCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

fn creds_store() -> credentials::File {
    credentials::File::new(DB_NAME)
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

    ctx.subscription_builder()
        .on_error(|_ctx, err| {
            eprintln!("Subscription failed: {err}");
            std::process::exit(1);
        })
        .subscribe(["SELECT * FROM user", "SELECT * FROM cell"]);

    let (mut rl, thread) = raylib::init()
        .size(720, 720) // jam hard constraint
        .title("hex pixel-war — raylib 6.0 + SpacetimeDB 2.6")
        .build();
    rl.set_target_fps(60);

    let grid = hexgrid::cells();
    let mut selected: usize = 1;
    let mut stroke_last: Option<u32> = None;

    while !rl.window_should_close() {
        // Pump all pending SpacetimeDB messages on the main thread.
        if let Err(e) = ctx.frame_tick() {
            eprintln!("frame_tick: {e}");
            break;
        }

        let mouse = rl.get_mouse_position();

        // Sent every frame: this doubles as the presence heartbeat that
        // keeps `last_seen` fresh, so we don't render a stale cursor.
        if ctx.try_identity().is_some() {
            let _ = ctx.reducers.set_pos(mouse.x, mouse.y);
        }

        if rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT) {
            if let Some(i) = palette_hit(mouse) {
                selected = i;
            }
        } else if rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT) && mouse.y < PALETTE_Y0 {
            if let Some((c, r)) = nearest_cell(mouse) {
                let id = id_of(c, r);
                if Some(id) != stroke_last {
                    let _ = ctx.reducers.paint_cell(c, r, PALETTE[selected]);
                    stroke_last = Some(id);
                }
            }
        }
        if rl.is_mouse_button_released(MouseButton::MOUSE_BUTTON_LEFT) {
            stroke_last = None;
        }

        let me = ctx.try_identity();
        let now = Timestamp::now();

        let cell_colors: HashMap<u32, u32> =
            ctx.db.cell().iter().map(|c| (c.id, c.color)).collect();

        let mut d = rl.begin_drawing(&thread);
        d.clear_background(Color::RAYWHITE);

        for &(col, row, center) in &grid {
            let fill = cell_colors
                .get(&id_of(col, row))
                .map_or(color_u32(BOARD_FILL), |&c| color_u32(c));
            d.draw_poly(center, 6, HEX_SIZE, 0.0, fill);
            d.draw_poly_lines_ex(center, 6, HEX_SIZE, 0.0, 1.0, color_u32(GRID_LINE));
        }

        for user in ctx
            .db
            .user()
            .iter()
            .filter(|u| is_present(u.last_seen, now) && Some(u.identity) != me)
        {
            draw_cursor(&mut d, Vector2::new(user.x, user.y), color_for_hex(&user.identity.to_hex().to_string()));
        }

        draw_palette(&mut d, selected);

        d.draw_text("hex pixel-war", 10, 10, 20, Color::DARKGRAY);
        d.draw_fps(640, 10);
    }
}

fn is_present(last_seen: Timestamp, now: Timestamp) -> bool {
    now.duration_since(last_seen)
        .is_some_and(|elapsed| elapsed < PRESENCE_TIMEOUT)
}
