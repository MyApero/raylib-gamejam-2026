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
    pub const MARGIN_GAP_TILES: i32 = 8;

    /// How far (degrees, either direction) a painted hue may stray from an
    /// unlocked inventory entry. The server is the actual enforcement
    /// point (lets the Hue slider nudge a shade without bloating the
    /// inventory with one row per nudge); the client mirrors it so the
    /// slider never offers a value the server would reject.
    pub const HUE_TOLERANCE: i32 = 7;

    /// F11 (flying gift): world-unit radius of the small circular drift
    /// around the gift's spawn point. Shared because both clients render
    /// the drift AND the server's `claim_gift` validates distance against
    /// that same live position — see `gift_drift_pos` in `world.rs`/the
    /// server's mirrored copy.
    pub const GIFT_DRIFT_RADIUS: f32 = 1.2;
    /// F11: seconds per full drift loop. Shared for the same reason as
    /// `GIFT_DRIFT_RADIUS`.
    pub const GIFT_DRIFT_PERIOD_SECS: f32 = 5.0;
    /// F11: max world-unit distance from the gift's CURRENT drifted
    /// position (not just its spawn point) a claim is accepted from —
    /// enforced server-side in `claim_gift`, and reused client-side as both
    /// the click/tap hitbox and the visual affordance radius, so "how close
    /// counts" always means the same thing on both sides.
    pub const GIFT_CLAIM_DIST: f32 = 3.0;

    /// F13 (Hexa event): cursors needed in the six fixed world-centre slots
    /// to ignite.
    /// Shared because the client mirrors the server's own detection to
    /// decide which cursors to snap onto hexagon-vertex render positions —
    /// see `hexa_clusters` in `world.rs`.
    pub const HEXA_SIZE: usize = 6;
    /// HEXA becomes available on reaching level 3.
    pub const HEXA_UNLOCK_LEVEL: u64 = 3;
    /// Distance from the world origin at which an eligible cursor joins the
    /// fixed central formation. Ordinary merging has its own independent
    /// cursor-to-cursor distance.
    pub const HEXA_RADIUS: f32 = 2.0;
    /// F13: one-time-per-player Hexa bonus, granted the first time a player
    /// is ever part of an ignition. Server-only (client just observes the
    /// XP appear on `User.xp`), kept here anyway next to `HEXA_SIZE`/
    /// `HEXA_RADIUS` since all three are the F13 "canonical constants" set.
    pub const XP_HEXA: u64 = 150;
}

/// Circular hue distance in degrees (handles the 359->0 wraparound). Server
/// and client must agree on what "close to an unlocked hue" means (Hue
/// slider tolerance, long-press ownership check).
pub fn hue_dist(a: u16, b: u16) -> i32 {
    let diff = (a as i32 - b as i32).unsigned_abs() as i32;
    diff.min(360 - diff)
}

/// Standard ease-in-out cubic curve, shared by every client-side motion
/// that needs the same slow-start / fast-middle / slow-finish timing.
pub fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t.powi(3)
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}
