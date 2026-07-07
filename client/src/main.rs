mod module_bindings;
use module_bindings::*;

use raylib::prelude::*;
use spacetimedb_sdk::{credentials, DbContext, Identity, Table, Timestamp};

/// Local SpacetimeDB instance (`spacetime start`).
const HOST: &str = "http://localhost:3000";
const DB_NAME: &str = "hexmerge";
const HEX_RADIUS: f32 = 28.0;
/// A user is drawn as long as they've sent a position update within this window.
/// Web clients poll instead of holding a live connection, so presence is judged
/// by recency rather than the `online` flag (which HTTP polling would flap on
/// every request).
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
        .subscribe(["SELECT * FROM user"]);

    let (mut rl, thread) = raylib::init()
        .size(720, 720) // jam hard constraint
        .title("hexmerge — raylib 6.0 + SpacetimeDB 2.6")
        .build();
    rl.set_target_fps(60);

    while !rl.window_should_close() {
        // Pump all pending SpacetimeDB messages on the main thread.
        if let Err(e) = ctx.frame_tick() {
            eprintln!("frame_tick: {e}");
            break;
        }

        // Sent every frame: this doubles as the presence heartbeat that
        // keeps `last_seen` fresh, so we don't render our own hexagon as stale.
        let mouse = rl.get_mouse_position();
        if ctx.try_identity().is_some() {
            let _ = ctx.reducers.set_pos(mouse.x, mouse.y);
        }

        let me = ctx.try_identity();
        let now = Timestamp::now();
        let mut d = rl.begin_drawing(&thread);
        d.clear_background(Color::RAYWHITE);
        for user in ctx.db.user().iter().filter(|u| is_present(u.last_seen, now)) {
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

fn is_present(last_seen: Timestamp, now: Timestamp) -> bool {
    now.duration_since(last_seen)
        .is_some_and(|elapsed| elapsed < PRESENCE_TIMEOUT)
}

/// Deterministic per-user hexagon color derived from the identity bytes.
fn color_for(identity: &Identity) -> Color {
    let bytes = identity.to_be_byte_array();
    let h = bytes
        .iter()
        .fold(7u32, |acc, b| acc.wrapping_mul(31).wrapping_add(*b as u32));
    Color::color_from_hsv((h % 360) as f32, 0.7, 0.9)
}
