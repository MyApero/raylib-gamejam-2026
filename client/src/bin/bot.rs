//! Headless bot that connects like a normal player and drives its position
//! along a fixed trajectory forever — ambient movement for the board so it
//! never looks empty. Meant to run under a restart-on-exit supervisor (see
//! `run-bots.sh`): any disconnect exits the process non-zero and the
//! supervisor reconnects it a moment later.
//!
//! Usage: `cargo run -p client --bin bot --release -- heart|hexagon|center|assist-N`
//! (`assist-1` through `assist-5` are the animated showcase bots; higher
//! numbers are stationary load-test clients.)
//!
//! Positions are WORLD CARTESIAN units (1.0 = one hex outer radius,
//! world origin = admin's slot-0 island center — see plan.md's geometry
//! spec / `world::axial_to_world`), not the old fixed-canvas pixel space.
//! `set_pos` has no ownership/paint-permission check, so a bot's cursor can
//! sit anywhere regardless of which island (if any) is under it.

#[path = "../module_bindings/mod.rs"]
mod module_bindings;
mod world_geometry {
    // Bot doesn't link raylib (headless), so it can't pull in the full
    // `client::world` module (raylib types in its public interface) just
    // for two constants — mirror the tiny bit of geometry actually needed.
    /// World-unit Euclidean distance from an island's center to its
    /// furthest cell, for `ISLAND_RADIUS` (15) fine hexes — see
    /// `world::constants::ISLAND_FIT_ZOOM`'s comment for the derivation.
    pub const ISLAND_EDGE_REACH: f32 = 26.0;
}
use module_bindings::*;

use spacetimedb_sdk::{credentials, DbContext};
use std::f32::consts::PI;
use std::time::{Duration, Instant};

const HOST: &str = "http://localhost:3000";
const DB_NAME: &str = "hexel";

/// `HEXEL_DB` overrides the target database, matching the game client. The
/// systemd units (`hexel-bot@.service`) don't set it, so production bots
/// keep hitting `hexel`.
fn db_name() -> String {
    std::env::var("HEXEL_DB").unwrap_or_else(|_| DB_NAME.to_string())
}
/// World origin — the admin island's slot-0 center (plan.md geometry spec).
const CENTER: (f32, f32) = (0.0, 0.0);
/// Heart/hexagon loop radius, world units: comfortably past
/// `ISLAND_EDGE_REACH` so the path traces the margin ring around the admin
/// island rather than crossing its painted tiles.
const PATH_RADIUS: f32 = world_geometry::ISLAND_EDGE_REACH + 10.0;
/// The heart is painted on the central community island, so keep it compact
/// and shift it toward the island's upper-right (screen) quadrant.
const HEART_RADIUS: f32 = 9.0;
const HEART_CENTER: (f32, f32) = (6.0, -6.0);
/// The "Merge with me!" center bot instead idles in a small loop close to
/// the world origin, inside the admin island's own territory — the
/// backlog's literal ask ("centre ilot should have a bot"), and easy for a
/// solo rater to walk straight up to from wherever they spawn.
const CENTER_LOOP_RADIUS: f32 = 5.0;
/// Seconds for one full lap of the trajectory.
const PERIOD_SECS: f32 = 12.0;
/// The five demo bots each own one of five corners of the central white
/// island, leaving its sixth corner free for the presenter. Their 12-second
/// cycle is: ease inward, hold together long enough to showcase HEXA, ease
/// outward, then pause at their respective corners.
const ASSIST_TRAVEL_SECS: f32 = 3.0;
const ASSIST_CENTER_HOLD_SECS: f32 = 3.0;
const ASSIST_CORNER_HOLD_SECS: f32 = 3.0;
const ASSIST_PERIOD_SECS: f32 = ASSIST_TRAVEL_SECS * 2.0 + ASSIST_CENTER_HOLD_SECS + ASSIST_CORNER_HOLD_SECS;
/// The first five assistants are the curated HEXA demo. Additional assistants
/// exist to populate a local world with many real client connections/islands
/// without flooding the server with cursor updates.
const SHOWCASE_ASSIST_BOTS: u16 = 5;
const MAX_ASSIST_BOTS: u16 = 5_000;
/// Position update rate — matches roughly what a human mouse-drag produces.
const TICK: Duration = Duration::from_millis(50);
/// The heart bot periodically starts over with only its fresh seed color.
const HEART_INVENTORY_RESET: Duration = Duration::from_secs(120);

#[derive(Clone, Copy)]
enum Shape {
    Heart,
    Hexagon,
    Center,
    /// The first five are independently authenticated showcase bots; higher
    /// numbers are stationary load-test clients, each with its own identity.
    Assist(u16),
}

impl Shape {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "heart" => Some(Self::Heart),
            "hexagon" => Some(Self::Hexagon),
            "center" => Some(Self::Center),
            _ => s
                .strip_prefix("assist-")
                .and_then(|n| n.parse().ok())
                .filter(|&n| (1..=MAX_ASSIST_BOTS).contains(&n))
                .map(Self::Assist),
        }
    }

    /// Distinct per-shape key so each bot keeps (and reuses across
    /// restarts) its own SpacetimeDB identity instead of colliding with
    /// the human client's or each other's.
    fn creds_key(self) -> String {
        match self {
            Self::Heart => "hexel-bot-heart".to_string(),
            Self::Hexagon => "hexel-bot-hexagon".to_string(),
            Self::Center => "hexel-bot-center".to_string(),
            Self::Assist(n) => format!("hexel-bot-assist-{n}"),
        }
    }

    /// The display name IS the on-screen callout — `world::draw_cursor_label`
    /// renders every online player's name above their cursor, so setting it
    /// to this string is the whole feature, no bot-specific client code.
    fn display_name(self) -> String {
        match self {
            Self::Heart => "Merge with me!".to_string(),
            Self::Hexagon => "hexagon-bot".to_string(),
            Self::Center => "Merge with me!".to_string(),
            Self::Assist(n) => format!("Hexa bot {n}"),
        }
    }

    /// Position at fraction `t` (0..1) around one lap, world cartesian units.
    fn position(self, t: f32) -> (f32, f32) {
        match self {
            Self::Heart => heart_position(t),
            Self::Hexagon => hexagon_position(t),
            Self::Center => center_position(t),
            Self::Assist(n) if n <= SHOWCASE_ASSIST_BOTS => assist_position(n, t),
            Self::Assist(n) => load_test_position(n),
        }
    }

    /// Load-test assistants deliberately set their cursor once, then stay
    /// quiet. Their connections still create and keep an island, which is
    /// what the renderer stress test needs; sending hundreds of `set_pos`
    /// reducers every 50 ms would primarily benchmark the server instead.
    fn is_stationary_load_client(self) -> bool {
        matches!(self, Self::Assist(n) if n > SHOWCASE_ASSIST_BOTS)
    }
}

/// Classic parametric heart curve (x = 16sin^3, y = 13cos-5cos2-2cos3-cos4),
/// scaled to fit the central community island, and flipped on y since
/// world-cartesian y still grows the same direction screen space did (see
/// `world::axial_to_world`) but the formula assumes math-up.
fn heart_position(t: f32) -> (f32, f32) {
    let a = t * 2.0 * PI;
    let x = 16.0 * a.sin().powi(3);
    let y = 13.0 * a.cos() - 5.0 * (2.0 * a).cos() - 2.0 * (3.0 * a).cos() - (4.0 * a).cos();
    let scale = HEART_RADIUS / 16.0;
    (HEART_CENTER.0 + x * scale, HEART_CENTER.1 - y * scale)
}

/// Same flat-top axial projection used by the clients. Heart coordinates are
/// local to the slot-0 community island.
fn world_to_axial(x: f32, y: f32) -> (i32, i32) {
    let qf = x / 1.5;
    let rf = y / 3f32.sqrt() - qf / 2.0;
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

/// Traces the outline of a regular hexagon at constant speed, one straight
/// edge at a time — fitting since the players themselves are hexagons.
fn hexagon_position(t: f32) -> (f32, f32) {
    let verts: [(f32, f32); 6] = std::array::from_fn(|i| {
        let a = i as f32 * PI / 3.0;
        (CENTER.0 + PATH_RADIUS * a.cos(), CENTER.1 + PATH_RADIUS * a.sin())
    });
    let edge_t = t * 6.0;
    let i = edge_t.floor() as usize % 6;
    let frac = edge_t.fract();
    let (x0, y0) = verts[i];
    let (x1, y1) = verts[(i + 1) % 6];
    (x0 + (x1 - x0) * frac, y0 + (y1 - y0) * frac)
}

/// Small idle circle right at the world origin (see `CENTER_LOOP_RADIUS`).
fn center_position(t: f32) -> (f32, f32) {
    let a = t * 2.0 * PI;
    (CENTER.0 + CENTER_LOOP_RADIUS * a.cos(), CENTER.1 + CENTER_LOOP_RADIUS * a.sin())
}

fn assist_position(n: u16, t: f32) -> (f32, f32) {
    // Use five of the actual six hexagon corners, rather than distributing
    // five points around a pentagon. Slot 5 remains visibly available for
    // the presenter.
    let angle = (n - 1) as f32 * PI / 3.0;
    let corner = (
        CENTER.0 + world_geometry::ISLAND_EDGE_REACH * angle.cos(),
        CENTER.1 + world_geometry::ISLAND_EDGE_REACH * angle.sin(),
    );
    let elapsed = t.fract() * ASSIST_PERIOD_SECS;
    let radius_fraction = if elapsed < ASSIST_TRAVEL_SECS {
        1.0 - shared::ease_in_out_cubic(elapsed / ASSIST_TRAVEL_SECS)
    } else if elapsed < ASSIST_TRAVEL_SECS + ASSIST_CENTER_HOLD_SECS {
        0.0
    } else if elapsed < ASSIST_TRAVEL_SECS * 2.0 + ASSIST_CENTER_HOLD_SECS {
        shared::ease_in_out_cubic((elapsed - ASSIST_TRAVEL_SECS - ASSIST_CENTER_HOLD_SECS) / ASSIST_TRAVEL_SECS)
    } else {
        1.0
    };
    (
        CENTER.0 + (corner.0 - CENTER.0) * radius_fraction,
        CENTER.1 + (corner.1 - CENTER.1) * radius_fraction,
    )
}

/// Spread stationary load-test cursors in a sunflower pattern outside the
/// central island. Their islands themselves are positioned server-side from
/// their slots, independently of these cursor coordinates.
fn load_test_position(n: u16) -> (f32, f32) {
    debug_assert!(n > SHOWCASE_ASSIST_BOTS);
    let index = (n - SHOWCASE_ASSIST_BOTS - 1) as f32;
    let angle = index * 2.399_963_1; // golden angle, avoids visible spokes
    let radius = PATH_RADIUS + 4.0 * index.sqrt();
    (CENTER.0 + radius * angle.cos(), CENTER.1 + radius * angle.sin())
}

fn reset_demo_account(ctx: &DbConnection) {
    let (reset_tx, reset_rx) = std::sync::mpsc::sync_channel(1);
    ctx.reducers
        .reset_account_then(move |_ctx, result| {
            let outcome = match result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(message)) => Err(message),
                Err(error) => Err(format!("internal reducer error: {error}")),
            };
            let _ = reset_tx.send(outcome);
        })
        .unwrap_or_else(|error| {
            eprintln!("Failed to send reset_account: {error}");
            std::process::exit(1);
        });
    match reset_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => eprintln!("Demo account reset confirmed"),
        Ok(Err(error)) => {
            eprintln!("reset_account rejected: {error}");
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!("Timed out waiting for reset_account: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assist_bots_hold_center_for_three_seconds_and_leave_sixth_corner_free() {
        assert_eq!(assist_position(1, 0.0), (world_geometry::ISLAND_EDGE_REACH, 0.0));
        assert_eq!(assist_position(1, 3.0 / PERIOD_SECS), CENTER);
        assert_eq!(assist_position(1, 5.9 / PERIOD_SECS), CENTER);
        assert_eq!(assist_position(1, 9.0 / PERIOD_SECS), (world_geometry::ISLAND_EDGE_REACH, 0.0));
        let unused_angle = 5.0 * PI / 3.0;
        let unused = (world_geometry::ISLAND_EDGE_REACH * unused_angle.cos(), world_geometry::ISLAND_EDGE_REACH * unused_angle.sin());
        assert!((1..=5).all(|n| assist_position(n, 0.0) != unused));
    }

    #[test]
    fn higher_numbered_assistants_are_stationary_load_clients() {
        assert!(Shape::Assist(6).is_stationary_load_client());
        assert!(!Shape::Assist(5).is_stationary_load_client());
        assert_ne!(load_test_position(6), load_test_position(7));
    }
}

fn main() {
    let shape = std::env::args()
        .nth(1)
        .as_deref()
        .and_then(Shape::parse)
        .unwrap_or_else(|| {
            eprintln!("usage: bot <heart|hexagon|center|assist-1..assist-{MAX_ASSIST_BOTS}>");
            std::process::exit(2);
        });

    let creds_store = move || credentials::File::new(shape.creds_key());

    let ctx = DbConnection::builder()
        .on_connect(move |_ctx, _identity, token| {
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
            std::process::exit(1);
        })
        .with_token(creds_store().load().expect("Error loading credentials"))
        .with_database_name(db_name())
        .with_uri(HOST)
        .build()
        .expect("Failed to connect");

    ctx.run_threaded();

    while ctx.try_identity().is_none() {
        std::thread::sleep(Duration::from_millis(20));
    }
    // Demo assistants should start each launched showcase from a clean
    // account and therefore receive a newly-rerolled starting hue. Keep
    // the ambient heart/hexagon/center bots persistent. Reducer calls are
    // asynchronous: wait for the server result before sending any position
    // updates, otherwise a rejected reset is silent and the movement loop
    // can begin with the previous run's hue.
    if matches!(shape, Shape::Assist(n) if n <= SHOWCASE_ASSIST_BOTS) {
        reset_demo_account(&ctx);
    }
    let _ = ctx.reducers.set_name(shape.display_name());
    let (initial_x, initial_y) = shape.position(0.0);
    let _ = ctx.reducers.set_pos(initial_x, initial_y);

    let start = Instant::now();
    let mut last_heart_reset = Instant::now();
    loop {
        let t = (start.elapsed().as_secs_f32() / PERIOD_SECS).fract();
        let (x, y) = shape.position(t);
        if !shape.is_stationary_load_client() {
            let _ = ctx.reducers.set_pos(x, y);
        }
        if matches!(shape, Shape::Heart) {
            if last_heart_reset.elapsed() >= HEART_INVENTORY_RESET {
                // reset_account clears all discovered colors and gives the
                // bot one fresh seed color. Do not set the brush here: a
                // successful merge is deliberately allowed to control it.
                reset_demo_account(&ctx);
                last_heart_reset = Instant::now();
            }
            let (q, r) = world_to_axial(x, y);
            let _ = ctx.reducers.paint_community_cell(q, r);
        }
        std::thread::sleep(TICK);
    }
}
