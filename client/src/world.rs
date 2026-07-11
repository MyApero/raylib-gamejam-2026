//! Shared geometry/color contract for the hex-island world, consumed
//! identically by the native (`main.rs`) and, from F5, web (`bin/web.rs`)
//! clients so they can never drift on hex layout, slot placement, cell id
//! packing or color decoding. Mirrors `server/src/lib.rs`'s `geometry` /
//! `constants` modules exactly — see plan.md "Geometry spec" / "Color spec".

use raylib::prelude::*;
use std::sync::OnceLock;

pub mod constants {
    pub const ISLAND_RADIUS: i32 = 13;
    pub const SLOT_SPACING: i32 = 29;
    pub const PRESENCE_TIMEOUT_SECS: i64 = 3;
    /// Client-side send-rate cap for `set_pos`; the server has no matching
    /// limit (cursor spam is cheap), this just avoids flooding the socket.
    pub const CURSOR_SEND_HZ: f32 = 20.0;
    pub const LEVEL_XP: u64 = 100;
}

pub fn level_of(xp: u64) -> u64 {
    xp / constants::LEVEL_XP
}

pub fn sat_cap(level: u64) -> u8 {
    (40 + 3 * level).min(100) as u8
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

/// Fine axial center of a coarse slot.
pub fn slot_center(q: i32, r: i32) -> (i32, i32) {
    (constants::SLOT_SPACING * q, constants::SLOT_SPACING * r)
}

/// True if the fine world cell `(q, r)` belongs to some island's interior
/// (occupied or not). Mirrors `server::geometry::in_any_island_territory`.
pub fn in_any_island_territory(q: i32, r: i32) -> bool {
    let (cq, cr) = cube_round(
        q as f32 / constants::SLOT_SPACING as f32,
        r as f32 / constants::SLOT_SPACING as f32,
    );
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

/// Filled+outlined flat-top hex at world `center` with world-unit `radius`
/// (normally 1.0; camera zoom handles on-screen scale).
pub fn draw_hex(d: &mut impl RaylibDraw, center: Vector2, radius: f32, fill: Color, line: Color) {
    d.draw_poly(center, 6, radius, 0.0, fill);
    d.draw_poly_lines_ex(center, 6, radius, 0.0, radius * 0.04, line);
}

/// Filled pointer/arrow at screen-space `m` (apex at the tip), for the
/// local player's own mouse cursor — drawn in screen space so its size is
/// constant regardless of camera zoom, unlike the world-space hex tiles.
pub fn draw_cursor(d: &mut impl RaylibDraw, m: Vector2, color: Color) {
    let tip = m;
    let left = Vector2::new(m.x, m.y + 18.0);
    let right = Vector2::new(m.x + 13.0, m.y + 13.0);
    d.draw_triangle(tip, left, right, color);
    d.draw_triangle_lines(tip, left, right, Color::BLACK);
}
