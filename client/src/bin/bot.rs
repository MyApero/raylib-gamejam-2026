//! Headless bot that connects like a normal player and drives its position
//! along a fixed trajectory forever — ambient movement for the board so it
//! never looks empty. Meant to run under a restart-on-exit supervisor (see
//! `run-bots.sh`): any disconnect exits the process non-zero and the
//! supervisor reconnects it a moment later.
//!
//! Usage: `cargo run -p client --bin bot --release -- heart|hexagon`

#[path = "../module_bindings/mod.rs"]
mod module_bindings;
use module_bindings::*;

use spacetimedb_sdk::{credentials, DbContext};
use std::f32::consts::PI;
use std::time::{Duration, Instant};

const HOST: &str = "http://localhost:3000";
const DB_NAME: &str = "hexmerge";
/// Canvas is a 720x720 square (see client/src/main.rs); keep the path well
/// clear of the edges and of each hexagon's own drawn radius (28px).
const CENTER: (f32, f32) = (360.0, 360.0);
const PATH_RADIUS: f32 = 220.0;
/// Seconds for one full lap of the trajectory.
const PERIOD_SECS: f32 = 12.0;
/// Position update rate — matches roughly what a human mouse-drag produces.
const TICK: Duration = Duration::from_millis(50);

#[derive(Clone, Copy)]
enum Shape {
    Heart,
    Hexagon,
}

impl Shape {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "heart" => Some(Self::Heart),
            "hexagon" => Some(Self::Hexagon),
            _ => None,
        }
    }

    /// Distinct per-shape key so each bot keeps (and reuses across
    /// restarts) its own SpacetimeDB identity instead of colliding with
    /// the human client's or each other's.
    fn creds_key(self) -> &'static str {
        match self {
            Self::Heart => "hexmerge-bot-heart",
            Self::Hexagon => "hexmerge-bot-hexagon",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Heart => "heart-bot",
            Self::Hexagon => "hexagon-bot",
        }
    }

    /// Position at fraction `t` (0..1) around one lap, in canvas pixels.
    fn position(self, t: f32) -> (f32, f32) {
        match self {
            Self::Heart => heart_position(t),
            Self::Hexagon => hexagon_position(t),
        }
    }
}

/// Classic parametric heart curve (x = 16sin^3, y = 13cos-5cos2-2cos3-cos4),
/// scaled so its 32-unit-wide bounding box fits PATH_RADIUS, and flipped on
/// y since screen space grows downward but the formula assumes math-up.
fn heart_position(t: f32) -> (f32, f32) {
    let a = t * 2.0 * PI;
    let x = 16.0 * a.sin().powi(3);
    let y = 13.0 * a.cos() - 5.0 * (2.0 * a).cos() - 2.0 * (3.0 * a).cos() - (4.0 * a).cos();
    let scale = PATH_RADIUS / 16.0;
    (CENTER.0 + x * scale, CENTER.1 - y * scale)
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

fn main() {
    let shape = std::env::args()
        .nth(1)
        .as_deref()
        .and_then(Shape::parse)
        .unwrap_or_else(|| {
            eprintln!("usage: bot <heart|hexagon>");
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
        .with_database_name(DB_NAME)
        .with_uri(HOST)
        .build()
        .expect("Failed to connect");

    ctx.run_threaded();

    while ctx.try_identity().is_none() {
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = ctx.reducers.set_name(shape.display_name().to_string());

    let start = Instant::now();
    loop {
        let t = (start.elapsed().as_secs_f32() / PERIOD_SECS).fract();
        let (x, y) = shape.position(t);
        let _ = ctx.reducers.set_pos(x, y);
        std::thread::sleep(TICK);
    }
}
