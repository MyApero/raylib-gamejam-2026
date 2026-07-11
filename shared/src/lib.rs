//! Constants and pure functions that the server (SpacetimeDB module,
//! wasm32-unknown-unknown) and both clients (native, and web via
//! wasm32-unknown-emscripten) must agree on byte-for-byte. This crate has
//! zero dependencies so it compiles cleanly under all three targets.
//!
//! Previously these were hand-copied into each side with a doc comment
//! promising they'd be kept in sync — `INTRO_DURATION` and
//! `BORDERLESS_ZOOM_THRESHOLD` (client-only, see `world.rs`) already drifted
//! that way once. This crate is the single source of truth for anything
//! actually shared between client and server; constants that are tuning
//! knobs for only one side (e.g. server's `PAINT_BUCKET_MAX`, client's
//! `CURSOR_SEND_HZ`) stay local to that side.

pub mod constants {
    /// Radius (in fine hex tiles) of an island's paintable interior —
    /// `hexdist <= ISLAND_RADIUS` from the island's own center. Single
    /// source of truth: previously hand-mirrored in both the server and
    /// client with a doc comment promising sync, which let them drift to
    /// 13 (client) vs 15 (server) after a radius bump only landed on one
    /// side.
    pub const ISLAND_RADIUS: i32 = 15;

    /// Gap (in fine hex tiles) left between neighboring islands' paintable
    /// interiors. MUST be even: the placement radius is bumped by
    /// `MARGIN_GAP_TILES / 2` on top of `ISLAND_RADIUS`, and that bump
    /// contributes to the gap symmetrically from both neighboring islands —
    /// so the resulting gap is always exactly `2 * bump`, i.e. always even.
    /// An odd value here is not geometrically reachable with this tiling
    /// and would silently round down via integer division.
    pub const MARGIN_GAP_TILES: i32 = 6;

    /// How far (degrees, either direction) a painted hue may stray from an
    /// unlocked inventory entry. The server is the actual enforcement
    /// point (lets the Hue slider nudge a shade without bloating the
    /// inventory with one row per nudge); the client mirrors it so the
    /// slider never offers a value the server would reject.
    pub const HUE_TOLERANCE: i32 = 7;
}

/// Circular hue distance in degrees (handles the 359->0 wraparound). Server
/// and client must agree on what "close to an unlocked hue" means (Hue
/// slider tolerance, long-press ownership check).
pub fn hue_dist(a: u16, b: u16) -> i32 {
    let diff = (a as i32 - b as i32).unsigned_abs() as i32;
    diff.min(360 - diff)
}
