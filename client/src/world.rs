//! Shared geometry/color contract for the hex-island world, consumed
//! identically by the native (`main.rs`) and, from F5, web (`bin/web.rs`)
//! clients so they can never drift on hex layout, slot placement, cell id
//! packing or color decoding. Mirrors `server/src/lib.rs`'s `geometry`
//! module exactly — see plan.md "Geometry spec" / "Color spec". The pieces
//! genuinely shared with the server (`MARGIN_GAP_TILES`, `HUE_TOLERANCE`,
//! `hue_dist`) live in the `shared` crate instead and are re-exported below,
//! rather than hand-mirrored.

use raylib::prelude::*;
use std::sync::OnceLock;

pub mod constants {
    use std::time::Duration;

    /// Gap (in fine hex tiles) left between neighboring islands' paintable
    /// interiors. Single source of truth in the `shared` crate — see its
    /// doc comment for why it MUST be even.
    pub use shared::constants::MARGIN_GAP_TILES;
    /// Single source of truth in the `shared` crate — see its doc comment
    /// for why this is no longer hand-mirrored.
    pub use shared::constants::ISLAND_RADIUS;
    /// Client-side send-rate cap for `set_pos`; the server has no matching
    /// limit (cursor spam is cheap), this just avoids flooding the socket.
    pub const CURSOR_SEND_HZ: f32 = 20.0;
    pub const LEVEL_XP: u64 = 100;
    /// How far (degrees, either direction) the Hue slider may nudge the
    /// selected inventory hue. Single source of truth in the `shared`
    /// crate — the server is the actual enforcement point, this just keeps
    /// the slider from offering a value the server would reject.
    pub use shared::constants::HUE_TOLERANCE;
    /// F9.5 item 6: floor on another player's cursor's zoomed-out render
    /// scale (relative to its size at the default `ISLAND_FIT_ZOOM`) — lets
    /// it shrink with the camera like a world-space object would, but never
    /// past "still findable" small.
    pub const CURSOR_MIN_SCALE: f32 = 0.4;

    // --- Shared UI tuning ---
    // Native (`main.rs`) and web (`bin/web.rs`) used to each declare their
    // own copies of these, which silently drifted apart (`INTRO_DURATION`
    // and `BORDERLESS_ZOOM_THRESHOLD` ended up with different values on
    // each client despite comments claiming they mirrored exactly). Single
    // source of truth now.

    /// Client-side cap on paint-reducer calls while dragging; the server's
    /// own token bucket (1000 tiles / 20 s) is the real limit, this just
    /// avoids spamming calls faster than a stroke can usefully register.
    pub const CLIENT_PAINT_HZ: f32 = 100.0;
    /// Zoom level used whenever the camera centers on the player's own
    /// island (startup and the footer's Center button): fits the 721-cell
    /// island (radius 15, so ~26.0 world units to the furthest edge) inside
    /// the 720x720 window with the header/footer bands and a little padding.
    /// Scaled down from the old radius-13 value (13.0) by 13/15 to keep the
    /// same on-screen fit — REASONED, not hand-verified; re-tune if the
    /// island looks clipped or too small after playtesting.
    pub const ISLAND_FIT_ZOOM: f32 = 11.267;
    /// Long-press-to-merge thresholds: hold LMB steady within
    /// `LONG_PRESS_TOL_PX` screen pixels for `LONG_PRESS_HOLD` to trigger
    /// `merge_with_cell`.
    pub const LONG_PRESS_HOLD: Duration = Duration::from_millis(400);
    pub const LONG_PRESS_TOL_PX: f32 = 8.0;
    /// Author-requested: two clean single-clicks landing on the SAME
    /// foreign island within this window (and without drifting past
    /// `LONG_PRESS_TOL_PX`) toggle a like/unlike instead of opening the
    /// info popup — see `pending_info_click`.
    pub const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(350);
    /// Author-reported (mobile hand-test, 2026-07-11): double-click-to-like
    /// wasn't registering on a phone. Root cause: the canvas is fixed at
    /// 720x720 internal render resolution (`client/web/game.html`'s
    /// `width: 100vmin`), but on most phone screens that's displayed well
    /// under 720 CSS px, so the browser upscales — any physical finger
    /// jitter between the two taps of a double-tap gets magnified by that
    /// same ratio once mapped into game-space coordinates.
    /// `LONG_PRESS_TOL_PX` (8px) was tuned for mouse precision and is far
    /// too tight for two independent finger contacts; this is a separate,
    /// more forgiving tolerance used ONLY to match the second tap's
    /// position against the first (the "is this the same click" check),
    /// not for the existing single-press hold-still/drag detection, which
    /// stays as-is.
    pub const DOUBLE_CLICK_TOL_PX: f32 = 28.0;
    /// F9.5 item 7 / decision 17: how long the cursor must sit continuously
    /// over a foreign island before its info popup opens on its own — long
    /// enough that a paint stroke's cursor briefly sweeping past a
    /// neighboring border doesn't flicker it open.
    pub const HOVER_OPEN_DELAY: Duration = Duration::from_millis(200);
    /// F9.6 item 2: middle-click eyedropper — a clean middle press+release
    /// within this tolerance/window is a "click"; drifting past it (or
    /// holding past the window without release) is the existing
    /// middle-drag PAN gesture instead, which stays completely unaffected
    /// since it's driven separately by `is_mouse_button_down` every frame
    /// regardless of this.
    pub const MIDDLE_CLICK_TOL_PX: f32 = 8.0;
    /// F9.6 item 7: how long the launch intro's ease from the whole-world
    /// view to the player's island takes, absent any input (which skips it
    /// instantly). Re-tuned by hand-testing 1750ms -> 3000ms (see status.md).
    pub const INTRO_DURATION: Duration = Duration::from_millis(3000);
    /// F9.6 item 8: on-screen hex size (world-unit radius 1.0 *
    /// `camera.zoom`, in pixels) below which the per-tile outline pass is
    /// skipped — the author's "borderless far zoom" note picked ~4-6px, the
    /// executor settled on 5, later re-tuned by hand-testing to 20.
    pub const BORDERLESS_ZOOM_THRESHOLD: f32 = 20.0;
    /// F9.6 item 6: keyboard pan speed, world units/sec at zoom 1.0
    /// (divided by the current zoom so it feels like a constant SCREEN
    /// speed, same trick as the border-thickness fix above). Q/E zoom rate
    /// is a fraction-per-second multiplier, chosen so a held key covers
    /// roughly the same range as a few mouse-wheel notches per second.
    pub const KEY_PAN_SPEED: f32 = 400.0;
    pub const KEY_ZOOM_RATE: f32 = 1.4;
    /// Author-requested: the hovered-tile highlight's outline should "act
    /// like the ilot border" — a constant SCREEN pixel width (divided by
    /// camera zoom at the draw site, same trick as the island border),
    /// instead of the fixed world-unit thickness it had before, which shrank
    /// under a pixel and vanished at low zoom.
    pub const HOVER_BORDER_PX: f32 = 2.0;

    /// F11 (flying gift): single source of truth in the `shared` crate —
    /// both the server's `claim_gift` distance check and this client's
    /// drift rendering must derive the identical position/range from it.
    pub use shared::constants::GIFT_DRIFT_RADIUS;
    pub use shared::constants::GIFT_DRIFT_PERIOD_SECS;
    /// F11: reused directly as both the click/tap hitbox (world-space, not
    /// converted from screen pixels, so "close enough" means the same thing
    /// here as it does server-side) and the visual affordance radius.
    pub use shared::constants::GIFT_CLAIM_DIST;
}

pub fn level_of(xp: u64) -> u64 {
    xp / constants::LEVEL_XP
}

pub fn sat_cap(level: u64) -> u8 {
    (40 + 3 * level).min(100) as u8
}

/// Circular hue distance in degrees (handles the 359->0 wraparound). Single
/// source of truth in the `shared` crate — both server and client must
/// agree on what "close to an unlocked hue" means (Hue slider tolerance,
/// long-press ownership check).
pub use shared::hue_dist;

/// Ease-in-out-cubic (author-requested for the launch intro: slow start,
/// accelerating through the middle, slowing again into the landing — not
/// the ease-OUT-cubic the intro originally shipped with, which was fast at
/// the start instead). Standard formula: two mirrored cubic curves, one per
/// half of `t`.
pub fn ease_in_out_cubic(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t.powi(3)
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// F11: flying-gift world position — a small circular drift around the
/// spawn point, a pure function of elapsed seconds since `Gift.spawned_at`.
/// Mirrors the server's own `gift_drift_pos` exactly (must derive the
/// identical position from the same inputs — no continuous position sync,
/// and `claim_gift` validates distance against this same live position).
pub fn gift_drift_pos(spawn: Vector2, elapsed_secs: f32) -> Vector2 {
    let angle = elapsed_secs / constants::GIFT_DRIFT_PERIOD_SECS * std::f32::consts::TAU;
    Vector2::new(spawn.x + constants::GIFT_DRIFT_RADIUS * angle.cos(), spawn.y + constants::GIFT_DRIFT_RADIUS * angle.sin())
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
/// deliberately bumped by `MARGIN_GAP_TILES / 2` over the real
/// `ISLAND_RADIUS` (a uniform `MARGIN_GAP_TILES`-tile gap, not zero).
const SLOT_PLACEMENT_RADIUS: i32 = constants::ISLAND_RADIUS + constants::MARGIN_GAP_TILES / 2;
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

/// All local `(q, r)` offsets of an island's 721-cell interior
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
/// the island's own center (see `ISLAND_RADIUS`'s ±15 range, offset by 16 to
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

/// F9.6 item 3: heart icon for the island popup/tooltip's like count —
/// filled (solid red) if the viewer has already liked the island, outline
/// (just the stroke) otherwise. Two lobes (circles) plus a downward-pointing
/// triangle, the standard heart-from-primitives composition.
pub fn draw_heart(d: &mut impl RaylibDraw, center: Vector2, size: f32, filled: bool, color: Color) {
    let lobe_r = size * 0.28;
    let lobe_y = center.y - size * 0.12;
    let left = Vector2::new(center.x - lobe_r * 0.95, lobe_y);
    let right = Vector2::new(center.x + lobe_r * 0.95, lobe_y);
    let top = Vector2::new(center.x, center.y + size * 0.55);
    let bl = Vector2::new(center.x - size * 0.5, center.y - size * 0.05);
    let br = Vector2::new(center.x + size * 0.5, center.y - size * 0.05);
    if filled {
        d.draw_circle_v(left, lobe_r, color);
        d.draw_circle_v(right, lobe_r, color);
        d.draw_triangle(bl, top, br, color);
    } else {
        d.draw_circle_lines(left.x as i32, left.y as i32, lobe_r, color);
        d.draw_circle_lines(right.x as i32, right.y as i32, lobe_r, color);
        d.draw_triangle_lines(bl, top, br, color);
    }
}

/// F9.6 item 1: small eraser badge near the screen-space cursor, shown
/// whenever paint/erase mode is toggled on — distinct from `draw_plus_hint`
/// (which only ever appears in paint mode, hovering a foreign tile), so the
/// two never compete for the same corner in practice.
pub fn draw_eraser_badge(d: &mut impl RaylibDraw, m: Vector2) {
    let cx = m.x + 18.0;
    let cy = m.y + 2.0;
    d.draw_circle(cx as i32, cy as i32, 8.0, Color::new(20, 20, 24, 220));
    d.draw_rectangle_lines(cx as i32 - 4, cy as i32 - 3, 8, 6, Color::RAYWHITE);
    d.draw_line_ex(Vector2::new(cx - 5.0, cy + 5.0), Vector2::new(cx + 5.0, cy - 5.0), 1.5, Color::new(230, 90, 90, 255));
}

/// F11: world-space icon for the flying-gift pickup — a rotated square
/// ("box") with a light cross ribbon, gently pulsing. Built from the same
/// primitives as `draw_hex`/`draw_plus_hint` (`draw_poly` + `draw_line_ex`)
/// rather than raylib's rectangle calls, since nothing else in this file
/// uses those yet. `elapsed_secs` (since the gift's spawn) drives the pulse,
/// the same "no continuous sync needed" trick `gift_drift_pos` uses for
/// position.
pub fn draw_gift_icon(d: &mut impl RaylibDraw, center: Vector2, elapsed_secs: f32) {
    let pulse = 1.0 + 0.10 * (elapsed_secs * 2.5).sin();
    let s = 0.5 * pulse;
    d.draw_circle_v(center, s * 1.8, Color::new(255, 215, 90, 50));
    d.draw_poly(center, 4, s, 45.0, Color::new(232, 90, 90, 255));
    d.draw_poly_lines_ex(center, 4, s, 45.0, s * 0.08, Color::new(60, 25, 25, 255));
    let ribbon = Color::new(255, 232, 130, 255);
    d.draw_line_ex(Vector2::new(center.x - s, center.y), Vector2::new(center.x + s, center.y), s * 0.22, ribbon);
    d.draw_line_ex(Vector2::new(center.x, center.y - s), Vector2::new(center.x, center.y + s), s * 0.22, ribbon);
}

/// Progress ring around the screen-space cursor while long-pressing toward a
/// merge (`frac` 0.0..1.0 of the hold threshold elapsed).
pub fn draw_hold_ring(d: &mut impl RaylibDraw, m: Vector2, frac: f32) {
    let center = Vector2::new(m.x + 6.0, m.y + 12.0);
    d.draw_ring(center, 10.0, 14.0, -90.0, -90.0 + 360.0 * frac.clamp(0.0, 1.0), 24, Color::new(255, 255, 255, 220));
}

/// Filled+outlined flat-top hex at world `center` with world-unit `radius`
/// (normally 1.0; camera zoom handles on-screen scale). `line: None` skips
/// the outline pass entirely — F9.6 item 8 (borderless far zoom): once the
/// on-screen hex size drops below a few pixels the outline is both a wasted
/// draw call and visual noise (the fill alone reads as a painting at that
/// distance), so the caller passes `None` past its own zoom threshold.
pub fn draw_hex(d: &mut impl RaylibDraw, center: Vector2, radius: f32, fill: Color, line: Option<Color>) {
    d.draw_poly(center, 6, radius, 0.0, fill);
    if let Some(line) = line {
        d.draw_poly_lines_ex(center, 6, radius, 0.0, radius * 0.04, line);
    }
}

/// Filled pointer/arrow at screen-space `m` (apex at the tip), for the local
/// player's own mouse cursor — always drawn at `scale` 1.0 so its size stays
/// constant regardless of camera zoom, matching the real mouse pointer.
pub fn draw_cursor(d: &mut impl RaylibDraw, m: Vector2, color: Color, locked: bool) {
    draw_cursor_scaled(d, m, color, 1.0, locked);
}

/// F9.5 item 6: other players' cursors used to always render at the same
/// fixed screen size as `draw_cursor`'s 1.0 scale, which reads as roughly
/// tile-sized at the default zoom players connect at but towers over the
/// tiles once zoomed out far — `scale` lets the caller shrink/grow it with
/// camera zoom instead (still screen-space geometry, just resized before
/// drawing), typically clamped to a minimum so it doesn't vanish either.
///
/// `locked` (author-requested): `set_lock`'s outline reads thicker — screen
/// pixels, not scaled by `scale`, so it stays a clearly-visible ring even on
/// a shrunk-down other-player cursor — so Lock state is visible at a glance
/// without opening anyone's info popup. Needs `draw_line_ex` per edge rather
/// than `draw_triangle_lines`, which has no thickness parameter.
pub fn draw_cursor_scaled(d: &mut impl RaylibDraw, m: Vector2, color: Color, scale: f32, locked: bool) {
    let tip = m;
    let left = Vector2::new(m.x, m.y + 18.0 * scale);
    let right = Vector2::new(m.x + 13.0 * scale, m.y + 13.0 * scale);
    d.draw_triangle(tip, left, right, color);
    let outline_px = if locked { 3.0 } else { 1.0 };
    d.draw_line_ex(tip, left, outline_px, Color::BLACK);
    d.draw_line_ex(left, right, outline_px, Color::BLACK);
    d.draw_line_ex(right, tip, outline_px, Color::BLACK);
}
