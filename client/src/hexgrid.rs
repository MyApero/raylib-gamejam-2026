//! Shared geometry/palette contract for the pixel-war whiteboard, consumed
//! identically by the native (`main.rs`) and web (`bin/web.rs`) clients so
//! they can never drift on hex layout, cell ids, or palette colors. The
//! server mirrors only the id-packing formula and the col/row bounds.

use raylib::prelude::*;

pub const HEX_SIZE: f32 = 22.0;
pub const COLS: u32 = 21;
pub const ROWS: u32 = 17;
const ORIGIN_X: f32 = 24.0;
const ORIGIN_Y: f32 = 24.0;
const DX: f32 = 1.5 * HEX_SIZE;
const DY: f32 = 38.105;

pub const PALETTE: [u32; 8] = [
    0xFFFFFF, 0x000000, 0xE43F3F, 0xF19E38, 0xF6D53B, 0x4FB84F, 0x3B82C4, 0x8E4FC0,
];

pub const PALETTE_Y0: f32 = 676.0;
pub const PALETTE_HEIGHT: f32 = 44.0;
pub const PALETTE_SWATCH_WIDTH: f32 = 90.0;

pub const BOARD_FILL: u32 = 0xF2F2F2;
pub const GRID_LINE: u32 = 0xDCDCDC;

pub fn id_of(col: u32, row: u32) -> u32 {
    (col << 16) | row
}

pub fn center(col: u32, row: u32) -> Vector2 {
    Vector2::new(
        ORIGIN_X + col as f32 * DX,
        ORIGIN_Y + row as f32 * DY + (col & 1) as f32 * DY / 2.0,
    )
}

pub fn cells() -> Vec<(u32, u32, Vector2)> {
    (0..COLS)
        .flat_map(|col| (0..ROWS).map(move |row| (col, row)))
        .map(|(col, row)| (col, row, center(col, row)))
        .collect()
}

/// Nearest precomputed center within `HEX_SIZE`. O(COLS*ROWS) per call, which
/// at 357 cells is free once a frame — simpler and more robust than
/// pixel-to-axial-hex rounding.
pub fn nearest_cell(m: Vector2) -> Option<(u32, u32)> {
    let mut best: Option<(u32, u32, f32)> = None;
    for col in 0..COLS {
        for row in 0..ROWS {
            let c = center(col, row);
            let d2 = (c.x - m.x).powi(2) + (c.y - m.y).powi(2);
            if best.is_none_or(|(_, _, best_d2)| d2 < best_d2) {
                best = Some((col, row, d2));
            }
        }
    }
    best.filter(|&(_, _, d2)| d2 <= HEX_SIZE * HEX_SIZE)
        .map(|(col, row, _)| (col, row))
}

pub fn palette_hit(m: Vector2) -> Option<usize> {
    if m.y >= PALETTE_Y0 {
        Some(((m.x / PALETTE_SWATCH_WIDTH) as usize).clamp(0, PALETTE.len() - 1))
    } else {
        None
    }
}

pub fn color_u32(c: u32) -> Color {
    Color::new(
        ((c >> 16) & 0xFF) as u8,
        ((c >> 8) & 0xFF) as u8,
        (c & 0xFF) as u8,
        255,
    )
}

/// Deterministic per-identity color for cursor triangles, shared by both
/// clients (native hashes raw identity bytes, web hashes the hex string —
/// this takes the string form so both can funnel through it).
pub fn color_for_hex(hex: &str) -> Color {
    let h = hex
        .bytes()
        .fold(7u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    Color::color_from_hsv((h % 360) as f32, 0.7, 0.9)
}

/// Filled pointer triangle for another player's cursor, apex at `m`.
pub fn draw_cursor(d: &mut RaylibDrawHandle, m: Vector2, color: Color) {
    d.draw_triangle(
        m,
        Vector2::new(m.x, m.y + 18.0),
        Vector2::new(m.x + 13.0, m.y + 13.0),
        color,
    );
    d.draw_triangle_lines(
        m,
        Vector2::new(m.x, m.y + 18.0),
        Vector2::new(m.x + 13.0, m.y + 13.0),
        Color::BLACK,
    );
}

/// The 8-swatch palette strip along the bottom, with `selected` highlighted.
pub fn draw_palette(d: &mut RaylibDrawHandle, selected: usize) {
    for (i, &swatch) in PALETTE.iter().enumerate() {
        let x = i as f32 * PALETTE_SWATCH_WIDTH;
        d.draw_rectangle_rec(
            Rectangle::new(x, PALETTE_Y0, PALETTE_SWATCH_WIDTH, PALETTE_HEIGHT),
            color_u32(swatch),
        );
        let border = if i == selected { Color::RED } else { Color::BLACK };
        let thick = if i == selected { 3.0 } else { 1.0 };
        d.draw_rectangle_lines_ex(
            Rectangle::new(x, PALETTE_Y0, PALETTE_SWATCH_WIDTH, PALETTE_HEIGHT),
            thick,
            border,
        );
    }
}
