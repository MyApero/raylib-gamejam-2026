//! Baked title-screen map.
//!
//! The title screen used to draw the live world behind its backdrop, which
//! meant rasterising every painted hex in the world each frame purely as
//! decoration. It draws this pre-rendered PNG instead — one textured quad —
//! and skips the cell loops entirely while the title is up. Live player
//! cursors are still drawn on top, so the screen keeps showing activity;
//! only the painted tiles are frozen.
//!
//! `title-map-view.txt` records the camera pose the PNG was rendered with.
//! The title screen adopts exactly that pose, so cursors positioned through
//! the normal world->screen camera transform land where they belong on the
//! baked pixels.
//!
//! Regenerate both files after the world changes enough to be worth it:
//!   tools/map-image/render_map.py hexel client/assets/title-map

use raylib::prelude::*;

const PNG: &[u8] = include_bytes!("../assets/title-map.png");
const VIEW: &str = include_str!("../assets/title-map-view.txt");

/// Camera pose the PNG was rendered with: `(target_x, target_y, zoom)`.
/// Panics on a malformed file — it is generated and compiled in, so a bad
/// parse is a build-time mistake, not a runtime condition to handle.
pub fn view() -> (Vector2, f32) {
    parse_view(VIEW).expect("title-map-view.txt is malformed")
}

/// Same fields, from a file fetched at runtime rather than compiled in — so
/// a malformed one is a condition to survive, not a panic. `None` leaves the
/// caller on the embedded bake.
pub fn parse_view(text: &str) -> Option<(Vector2, f32)> {
    let mut parts = text.split_whitespace().map(|n| n.parse::<f32>().ok());
    let x = parts.next()??;
    let y = parts.next()??;
    let zoom = parts.next()??;
    zoom.is_finite()
        .then_some((Vector2::new(x, y), zoom))
        .filter(|_| x.is_finite() && y.is_finite() && zoom > 0.0)
}

pub fn parse_baked_at(text: &str) -> i64 {
    text.split_whitespace()
        .nth(3)
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Newest `painted_at` (micros since epoch) contained in the bake.
///
/// The image is baked offline and compiled in, so it never changes at
/// runtime — without this, a player could paint, zoom out, and not see their
/// own work. Cells newer than this are drawn live on top of the image, which
/// is cheap because there are only ever a handful of them.
pub fn baked_at_micros() -> i64 {
    parse_baked_at(VIEW)
}

/// Render size the PNG was baked at, and the screen it fills 1:1 at the
/// title pose. Only used to recover the world extent from the stored zoom.
const BAKED_PX: f32 = 720.0;

/// Zoom at or below which the baked image is drawn alone (no live cells).
/// This is the range where drawing every hex costs the most — the whole
/// world is in view — and where the bake is indistinguishable from it.
const IMAGE_ONLY_ZOOM: f32 = 4.0;
/// Zoom at or above which only live cells draw: close enough in that the
/// bake's ~28px-per-island resolution would read as blurry.
const LIVE_ONLY_ZOOM: f32 = 8.0;

/// World-space rectangle the image covers. Drawing it through the camera
/// (inside `begin_mode2D`) rather than as a screen quad means it pans and
/// zooms with everything else, so it can stay up through the launch intro
/// instead of only at the frozen title pose.
pub fn world_rect_for((target, zoom): (Vector2, f32)) -> Rectangle {
    let extent = BAKED_PX / (2.0 * zoom);
    Rectangle::new(
        target.x - extent,
        target.y - extent,
        extent * 2.0,
        extent * 2.0,
    )
}

pub fn world_rect() -> Rectangle {
    world_rect_for(view())
}

/// How opaque the baked map should be at `zoom`: 1.0 far out, 0.0 once
/// zoomed in, crossfading between. Lets the intro ease from the cheap baked
/// world to the real one without a visible pop.
pub fn blend_alpha(zoom: f32) -> f32 {
    ((LIVE_ONLY_ZOOM - zoom) / (LIVE_ONLY_ZOOM - IMAGE_ONLY_ZOOM)).clamp(0.0, 1.0)
}

/// `None` if the image can't be decoded or uploaded — the caller falls back
/// to a plain background, which is a degraded title screen but still a
/// playable game.
pub fn load(rl: &mut RaylibHandle, thread: &RaylibThread) -> Option<Texture2D> {
    load_png(rl, thread, PNG)
}

/// Same, for a PNG fetched at runtime instead of the compiled-in one. The
/// embedded bake ships with the wasm so the title has a backdrop instantly;
/// this replaces it once a fresher render arrives, which is what keeps island
/// placement current without rebuilding the bundle on every re-rank.
pub fn load_png(rl: &mut RaylibHandle, thread: &RaylibThread, png: &[u8]) -> Option<Texture2D> {
    let image = Image::load_image_from_mem(".png", png)
        .map_err(|e| eprintln!("title map decode failed: {e}"))
        .ok()?;
    rl.load_texture_from_image(thread, &image)
        .map_err(|e| eprintln!("title map upload failed: {e}"))
        .ok()
}
