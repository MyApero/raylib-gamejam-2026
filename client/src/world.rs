//! Shared geometry/color contract for the hex-island world, consumed
//! identically by the native (`main.rs`) and, from F5, web (`bin/web.rs`)
//! clients so they can never drift on hex layout, slot placement, cell id
//! packing or color decoding. Mirrors `server/src/lib.rs`'s `geometry` /
//! `constants` modules exactly — see plan.md "Geometry spec" / "Color spec".

use raylib::prelude::*;
use std::sync::OnceLock;

pub mod constants {
    pub const ISLAND_RADIUS: i32 = 13;
    /// Client-side send-rate cap for `set_pos`; the server has no matching
    /// limit (cursor spam is cheap), this just avoids flooding the socket.
    pub const CURSOR_SEND_HZ: f32 = 20.0;
    pub const LEVEL_XP: u64 = 100;
    /// How far (degrees, either direction) the Hue slider may nudge the
    /// selected inventory hue — mirrors `server::constants::HUE_TOLERANCE`,
    /// which is the actual enforcement point; this just keeps the slider
    /// from offering a value the server would reject.
    pub const HUE_TOLERANCE: i32 = 5;
    /// F9.5 item 6: floor on another player's cursor's zoomed-out render
    /// scale (relative to its size at the default `ISLAND_FIT_ZOOM`) — lets
    /// it shrink with the camera like a world-space object would, but never
    /// past "still findable" small.
    pub const CURSOR_MIN_SCALE: f32 = 0.4;
}

pub fn level_of(xp: u64) -> u64 {
    xp / constants::LEVEL_XP
}

pub fn sat_cap(level: u64) -> u8 {
    (40 + 3 * level).min(100) as u8
}

/// Circular hue distance in degrees (handles the 359->0 wraparound). Mirrors
/// `server::hue_dist` exactly — both sides must agree on what "close to an
/// unlocked hue" means (Hue slider tolerance, long-press ownership check).
pub fn hue_dist(a: u16, b: u16) -> i32 {
    let diff = (a as i32 - b as i32).unsigned_abs() as i32;
    diff.min(360 - diff)
}

pub fn hexdist(dq: i32, dr: i32) -> i32 {
    (dq.abs() + dr.abs() + (dq + dr).abs()) / 2
}

/// Fractional axial -> nearest integer axial (standard cube-round).
pub fn cube_round(qf: f32, rf: f32) -> (i32, i32) {
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

/// E, SE, NW, W, SW, NE.
pub const DIRECTIONS: [(i32, i32); 6] = [(1, 0), (1, -1), (0, -1), (-1, 0), (-1, 1), (0, 1)];

/// Hex-of-hexes tiling basis (F9.5) — mirrors `server::geometry`'s
/// `SLOT_PLACEMENT_RADIUS`/`SLOT_U`/`SLOT_V`/`SLOT_DET` exactly; see their
/// comments there for why this (not a naive per-axis scale) is what makes
/// neighboring islands share a flat edge, and why the placement radius is
/// deliberately bumped by 1 over the real `ISLAND_RADIUS` (a uniform 2-tile
/// gap, not zero).
const SLOT_PLACEMENT_RADIUS: i32 = constants::ISLAND_RADIUS + 1;
const SLOT_U: (i32, i32) = (SLOT_PLACEMENT_RADIUS, SLOT_PLACEMENT_RADIUS + 1);
const SLOT_V: (i32, i32) = (-(SLOT_PLACEMENT_RADIUS + 1), 2 * SLOT_PLACEMENT_RADIUS + 1);
const SLOT_DET: i32 = SLOT_U.0 * SLOT_V.1 - SLOT_U.1 * SLOT_V.0;

/// slot_index -> coarse axial (Q, R). Slot 0 = admin at the origin; slots
/// 1.. spiral out over concentric coarse rings. Must match
/// `server::geometry::slot_coords` exactly.
pub fn slot_coords(slot_index: u32) -> (i32, i32) {
    if slot_index == 0 {
        return (0, 0);
    }
    let mut remaining = slot_index;
    let mut ring: i32 = 1;
    loop {
        let ring_size = (6 * ring) as u32;
        if remaining <= ring_size {
            break;
        }
        remaining -= ring_size;
        ring += 1;
    }
    let (sq, sr) = DIRECTIONS[4];
    let mut q = sq * ring;
    let mut r = sr * ring;
    let mut steps_left = remaining - 1;
    'outer: for &(dq, dr) in DIRECTIONS.iter() {
        for _ in 0..ring {
            if steps_left == 0 {
                break 'outer;
            }
            q += dq;
            r += dr;
            steps_left -= 1;
        }
    }
    (q, r)
}

/// Fine axial center of a coarse slot — `q * SLOT_U + r * SLOT_V` (a proper
/// 2D linear combination, not independent per-axis scaling). Mirrors
/// `server::geometry::slot_center` exactly.
pub fn slot_center(q: i32, r: i32) -> (i32, i32) {
    (q * SLOT_U.0 + r * SLOT_V.0, q * SLOT_U.1 + r * SLOT_V.1)
}

/// True if the fine world cell `(q, r)` belongs to some island's interior
/// (occupied or not). Mirrors `server::geometry::in_any_island_territory`
/// exactly, including the 2x2 matrix inverse (the tiling basis isn't
/// axis-aligned, so this can't be independent per-axis division).
pub fn in_any_island_territory(q: i32, r: i32) -> bool {
    let det = SLOT_DET as f32;
    let qf = (SLOT_V.1 as f32 * q as f32 - SLOT_V.0 as f32 * r as f32) / det;
    let rf = (-SLOT_U.1 as f32 * q as f32 + SLOT_U.0 as f32 * r as f32) / det;
    let (cq, cr) = cube_round(qf, rf);
    for &(dq, dr) in std::iter::once(&(0, 0)).chain(DIRECTIONS.iter()) {
        let (sq, sr) = (cq + dq, cr + dr);
        let (cx, cy) = slot_center(sq, sr);
        if hexdist(q - cx, r - cy) <= constants::ISLAND_RADIUS {
            return true;
        }
    }
    false
}

/// All local `(q, r)` offsets of an island's 547-cell interior
/// (`hexdist <= ISLAND_RADIUS`), computed once and cached.
pub fn island_offsets() -> &'static [(i32, i32)] {
    static OFFSETS: OnceLock<Vec<(i32, i32)>> = OnceLock::new();
    OFFSETS.get_or_init(|| {
        let r = constants::ISLAND_RADIUS;
        let mut v = Vec::new();
        for q in -r..=r {
            for rr in -r..=r {
                if hexdist(q, rr) <= r {
                    v.push((q, rr));
                }
            }
        }
        v
    })
}

/// World cartesian center of a fine axial hex (1.0 = hex outer radius; flat-top).
pub fn axial_to_world(q: i32, r: i32) -> Vector2 {
    Vector2::new(1.5 * q as f32, 3f32.sqrt() * (r as f32 + q as f32 / 2.0))
}

/// World cartesian -> nearest fine axial hex.
pub fn world_to_axial(p: Vector2) -> (i32, i32) {
    let qf = p.x / 1.5;
    let rf = p.y / 3f32.sqrt() - qf / 2.0;
    cube_round(qf, rf)
}

/// Mirrors `server::geometry::island_cell_id` exactly — lets both clients do
/// an O(1) point lookup by packed id instead of scanning every painted cell
/// in the world to find one island's. `q_local`/`r_local` are relative to
/// the island's own center (see `ISLAND_RADIUS`'s ±13 range, offset by 16 to
/// stay non-negative in the 5-bit field).
pub fn island_cell_id(island_id: u32, q_local: i32, r_local: i32) -> u32 {
    (island_id << 10) | (((q_local + 16) as u32) << 5) | ((r_local + 16) as u32)
}

pub fn unpack_hsv(color: u32) -> (u16, u8, u8) {
    (((color >> 16) & 0x1FF) as u16, ((color >> 8) & 0xFF) as u8, (color & 0xFF) as u8)
}

#[allow(dead_code)]
pub fn pack_hsv(h: u16, s: u8, v: u8) -> u32 {
    ((h as u32) << 16) | ((s as u32) << 8) | (v as u32)
}

pub fn hsv_color(h: u16, s: u8, v: u8) -> Color {
    Color::color_from_hsv(h as f32, s as f32 / 100.0, v as f32 / 100.0)
}

/// Small "+" badge near the screen-space cursor, shown while hovering a
/// long-press-eyedropper-eligible tile (painted, someone else's, hue not
/// already in the caller's inventory) — signals "you can pick this up".
pub fn draw_plus_hint(d: &mut impl RaylibDraw, m: Vector2) {
    let cx = m.x + 18.0;
    let cy = m.y + 2.0;
    d.draw_circle(cx as i32, cy as i32, 8.0, Color::new(20, 20, 24, 220));
    d.draw_line_ex(Vector2::new(cx - 4.0, cy), Vector2::new(cx + 4.0, cy), 2.0, Color::RAYWHITE);
    d.draw_line_ex(Vector2::new(cx, cy - 4.0), Vector2::new(cx, cy + 4.0), 2.0, Color::RAYWHITE);
}

/// Progress ring around the screen-space cursor while long-pressing toward a
/// merge (`frac` 0.0..1.0 of the hold threshold elapsed).
pub fn draw_hold_ring(d: &mut impl RaylibDraw, m: Vector2, frac: f32) {
    let center = Vector2::new(m.x + 6.0, m.y + 12.0);
    d.draw_ring(center, 10.0, 14.0, -90.0, -90.0 + 360.0 * frac.clamp(0.0, 1.0), 24, Color::new(255, 255, 255, 220));
}

/// Filled+outlined flat-top hex at world `center` with world-unit `radius`
/// (normally 1.0; camera zoom handles on-screen scale).
pub fn draw_hex(d: &mut impl RaylibDraw, center: Vector2, radius: f32, fill: Color, line: Color) {
    d.draw_poly(center, 6, radius, 0.0, fill);
    d.draw_poly_lines_ex(center, 6, radius, 0.0, radius * 0.04, line);
}

/// Filled pointer/arrow at screen-space `m` (apex at the tip), for the local
/// player's own mouse cursor — always drawn at `scale` 1.0 so its size stays
/// constant regardless of camera zoom, matching the real mouse pointer.
pub fn draw_cursor(d: &mut impl RaylibDraw, m: Vector2, color: Color) {
    draw_cursor_scaled(d, m, color, 1.0);
}

/// F9.5 item 6: other players' cursors used to always render at the same
/// fixed screen size as `draw_cursor`'s 1.0 scale, which reads as roughly
/// tile-sized at the default zoom players connect at but towers over the
/// tiles once zoomed out far — `scale` lets the caller shrink/grow it with
/// camera zoom instead (still screen-space geometry, just resized before
/// drawing), typically clamped to a minimum so it doesn't vanish either.
pub fn draw_cursor_scaled(d: &mut impl RaylibDraw, m: Vector2, color: Color, scale: f32) {
    let tip = m;
    let left = Vector2::new(m.x, m.y + 18.0 * scale);
    let right = Vector2::new(m.x + 13.0 * scale, m.y + 13.0 * scale);
    d.draw_triangle(tip, left, right, color);
    d.draw_triangle_lines(tip, left, right, Color::BLACK);
}
