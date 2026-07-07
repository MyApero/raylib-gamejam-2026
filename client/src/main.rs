mod module_bindings;
use module_bindings::*;

use raylib::prelude::*;
use spacetimedb_sdk::{credentials, DbContext, Identity, Table};

/// Local SpacetimeDB instance (`spacetime start`).
const HOST: &str = "http://localhost:3000";
const DB_NAME: &str = "hexmerge";
const HEX_RADIUS: f32 = 28.0;

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
        .subscribe(["SELECT * FROM user"]);

    let (mut rl, thread) = raylib::init()
        .size(720, 720) // jam hard constraint
        .title("hexmerge — raylib 6.0 + SpacetimeDB 2.6")
        .build();
    rl.set_target_fps(60);

    let mut last_sent: Option<(f32, f32)> = None;

    while !rl.window_should_close() {
        // Pump all pending SpacetimeDB messages on the main thread.
        if let Err(e) = ctx.frame_tick() {
            eprintln!("frame_tick: {e}");
            break;
        }

        let mouse = rl.get_mouse_position();
        let moved = last_sent
            .map_or(true, |(x, y)| (x - mouse.x).abs() > 0.5 || (y - mouse.y).abs() > 0.5);
        if moved && ctx.try_identity().is_some() {
            if ctx.reducers.set_pos(mouse.x, mouse.y).is_ok() {
                last_sent = Some((mouse.x, mouse.y));
            }
        }

        let me = ctx.try_identity();
        let mut d = rl.begin_drawing(&thread);
        d.clear_background(Color::RAYWHITE);
        for user in ctx.db.user().iter().filter(|u| u.online) {
            let center = Vector2::new(user.x, user.y);
            d.draw_poly(center, 6, HEX_RADIUS, 0.0, color_for(&user.identity));
            if Some(user.identity) == me {
                d.draw_poly_lines_ex(center, 6, HEX_RADIUS, 0.0, 3.0, Color::BLACK);
            }
        }
        d.draw_text("hex + merge — PoC", 10, 10, 20, Color::DARKGRAY);
        d.draw_fps(640, 10);
    }
}

/// Deterministic per-user hexagon color derived from the identity bytes.
fn color_for(identity: &Identity) -> Color {
    let bytes = identity.to_be_byte_array();
    let h = bytes
        .iter()
        .fold(7u32, |acc, b| acc.wrapping_mul(31).wrapping_add(*b as u32));
    Color::color_from_hsv((h % 360) as f32, 0.7, 0.9)
}
