//! Immediate-mode HUD: header, footer, and the inventory/color-picker
//! overlay. Raylib primitives only — no new dependencies. `main.rs` calls
//! `handle_input` (before `begin_drawing`, since text/mouse input needs
//! `&mut RaylibHandle`) then `draw` (during `begin_drawing`). Layout
//! constants here are the shared source of truth for the header/footer band
//! heights so the map viewport can size around them.

use raylib::prelude::*;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::world;

const TOAST_DURATION: Duration = Duration::from_millis(2500);
/// How long the Reset Account button stays armed after a first click, before
/// a second click is required to actually fire the reducer.
const RESET_CONFIRM_WINDOW: Duration = Duration::from_secs(4);
/// Cap on the Account overlay's import field. Author-caught (F9.5): this used
/// to be 256, which silently truncated every real SpacetimeDB reconnect token
/// (observed ~386 chars) into a corrupt JWT — the server then rejected it and
/// minted a fresh anonymous identity instead, which is exactly what "import
/// creates a new account instead of recovering" looked like. Generous
/// headroom over any observed token length, not a tightly-fitted bound.
const IMPORT_TOKEN_MAX_LEN: usize = 2048;

pub const HEADER_H: f32 = 28.0;
/// Author-requested: back to a single footer row now that Eraser/Lock are
/// icon-only and fit to the left of the name field again. Both
/// `main.rs`/`bin/web.rs` derive the map viewport from this constant, so
/// nothing else needed to change.
pub const FOOTER_H: f32 = 44.0;
const SCREEN_W: f32 = 720.0;
const SCREEN_H: f32 = 720.0;

fn point_in(p: Vector2, r: Rectangle) -> bool {
    p.x >= r.x && p.x <= r.x + r.width && p.y >= r.y && p.y <= r.y + r.height
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Hue,
    Sat,
    Val,
}

/// Signed circular offset of `current` from `base`, in `(-180, 180]`.
fn hue_offset_signed(current: u16, base: u16) -> i32 {
    let mut diff = current as i32 - base as i32;
    if diff > 180 {
        diff -= 360;
    } else if diff < -180 {
        diff += 360;
    }
    diff
}

/// "New color obtained" feedback for a just-inserted `inventory` row of the
/// caller's own — a toast line plus a fading flash of the new hue. `hue:
/// None` (F9.6 item 2's plain info toasts) just skips the flash swatch.
struct Toast {
    text: String,
    hue: Option<u16>,
    shown_at: Instant,
}

pub struct UiState {
    pub name_input: String,
    name_loaded: bool,
    name_focused: bool,
    pub overlay_open: bool,
    pub last3: VecDeque<u16>,
    dragging: Drag,
    toast: Option<Toast>,
    /// Anchor hue for the Hue slider's ±`HUE_TOLERANCE` window. Set exactly
    /// on a swatch click; otherwise auto-recentered (see `handle_input`)
    /// whenever the server's actual brush hue drifts outside that window —
    /// which is how it silently follows merges (cursor- or tile-) without
    /// needing `main.rs` to know anything about it.
    base_hue: u16,
    /// The hue an explicit swatch click most recently asked the server to
    /// set, until `info.brush.0` echoes it back. Without this, the one frame
    /// between the click (which sets `base_hue` immediately) and the
    /// `set_brush` round trip landing would compare the NEW anchor against
    /// the OLD confirmed brush hue — for two distant colors that's a huge
    /// jump, which both flickers `base_hue` back toward the old selection
    /// (via the resync check) and snaps the slider handle to an extreme for
    /// one frame before it settles at 0.
    pending_select: Option<u16>,
    /// Account overlay (F6: copy/import ID, reset account) — mutually
    /// exclusive with `overlay_open`, same modal footprint.
    pub account_open: bool,
    import_input: String,
    import_focused: bool,
    /// Set on the first click of "Reset account"; a second click within
    /// `RESET_CONFIRM_WINDOW` actually fires it, otherwise it auto-disarms.
    reset_armed_at: Option<Instant>,
    /// Brief "copied" acknowledgement after the Copy ID button is clicked —
    /// the JS clipboard call is fire-and-forget from Rust's side (no success
    /// signal comes back), so this just confirms the click registered.
    copy_clicked_at: Option<Instant>,
    /// F8 island-info popup, opened by the caller (`main.rs`/`bin/web.rs`)
    /// when a short click lands on a foreign island's center. Mutually
    /// exclusive with `overlay_open`/`account_open`, same modal footprint.
    pub island_popup: Option<IslandInfo>,
    /// Author-requested: floating "+1"/"-1" feedback for a double-click
    /// like/unlike on the map, screen-space so it survives camera pans
    /// without recomputing a world->screen projection every frame. Pruned in
    /// `handle_input` (same place `toast` expires), drawn in `draw`.
    like_anims: Vec<LikeAnim>,
    /// F9: digits-only edit buffer for the island-info popup's "set your
    /// link" field, seeded from the current `itch_rate_id` (if any) each time
    /// the popup opens on the caller's own island — see `open_island_info`.
    link_edit_input: String,
    link_edit_focused: bool,
    /// F9.6 item 1: paint/erase mode toggle — footer button or the `X` key
    /// (not `E`, which item 6 claims for keyboard zoom-in).
    pub eraser_on: bool,
    /// F9.6 item 5: minimal keybindings/help overlay, opened by Escape when
    /// nothing else is open (closed by Escape again, matching item 4's rule
    /// for every other overlay). Mutually exclusive with the other three.
    pub help_open: bool,
}

/// One floating like/unlike pop — see `UiState::spawn_like_anim`.
struct LikeAnim {
    pos: Vector2,
    liked: bool,
    started_at: Instant,
}

const LIKE_ANIM_DURATION: Duration = Duration::from_millis(600);

/// Everything the F8 island-info popup needs to render, precomputed by the
/// caller so this module stays free of SDK/DB types (mirrors `HudInfo`'s
/// `short_id` convention).
pub struct IslandInfo {
    pub island_id: u32,
    pub owner_label: String,
    pub likes: u32,
    /// Human-readable relative age (e.g. "3h ago"), precomputed by the
    /// caller since this module has no notion of `Timestamp`.
    pub age_label: String,
    pub link_id: Option<u32>,
    pub is_own: bool,
    pub already_liked: bool,
}

impl UiState {
    pub fn new() -> Self {
        Self {
            name_input: String::new(),
            name_loaded: false,
            name_focused: false,
            overlay_open: false,
            last3: VecDeque::new(),
            dragging: Drag::None,
            toast: None,
            base_hue: 0,
            pending_select: None,
            account_open: false,
            import_input: String::new(),
            import_focused: false,
            reset_armed_at: None,
            copy_clicked_at: None,
            island_popup: None,
            like_anims: Vec::new(),
            link_edit_input: String::new(),
            link_edit_focused: false,
            eraser_on: false,
            help_open: false,
        }
    }

    /// F9.6 item 6: whether a text field currently owns keyboard input —
    /// `main.rs`/`bin/web.rs` check this before letting WASD/arrows/Q/E pan
    /// or zoom the camera, so typing a name doesn't also drive it.
    pub fn text_field_focused(&self) -> bool {
        self.name_focused || self.import_focused || self.link_edit_focused
    }

    /// Author-requested: called by the caller right after firing
    /// `like_island`/`unlike_island` from a map double-click, so the click
    /// gets a little visual acknowledgement even though the popup never
    /// opens for that gesture. `pos` is the click's screen position.
    pub fn spawn_like_anim(&mut self, pos: Vector2, liked: bool) {
        self.like_anims.push(LikeAnim { pos, liked, started_at: Instant::now() });
    }

    /// Opens the F8 island-info popup, closing the other two (mutually
    /// exclusive) overlays if either was open.
    pub fn open_island_info(&mut self, info: IslandInfo) {
        self.overlay_open = false;
        self.account_open = false;
        self.help_open = false;
        // F9: seed the link edit field from the current value every time the
        // popup (re)opens on your own island, so editing starts from what's
        // actually set rather than whatever was last typed.
        self.link_edit_input = if info.is_own { info.link_id.map_or(String::new(), |id| id.to_string()) } else { String::new() };
        self.link_edit_focused = false;
        self.island_popup = Some(info);
    }

    /// Re-reads the two fields that can change while the popup sits open
    /// (author-caught: the Like button used to look stuck on "Like" after a
    /// successful like, because the popup was a one-time snapshot from the
    /// moment it opened — nothing ever told it the reducer had landed).
    /// Called every frame the popup is open, same as `HudInfo` is rebuilt
    /// fresh from server state every frame elsewhere in this module.
    pub fn refresh_island_popup(&mut self, likes: u32, already_liked: bool) {
        if let Some(popup) = &mut self.island_popup {
            popup.likes = likes;
            popup.already_liked = already_liked;
        }
    }

    /// Called by `main.rs` when it sees a fresh `inventory` row belonging to
    /// the caller (cursor- or tile-merge). `partner_label` is the other
    /// player's name if set, else their short identity hex.
    pub fn show_merge_toast(&mut self, hue: u16, partner_label: &str) {
        self.toast = Some(Toast {
            text: format!("new color, obtained with {partner_label}"),
            hue: Some(hue),
            shown_at: Instant::now(),
        });
        self.note_used_hue(hue);
    }

    /// F9 level-up feedback. `current_hue` just drives the toast's flash
    /// swatch (reusing `Toast`'s existing rendering) — a level-up has no
    /// color of its own the way a merge does. The Saturation slider's own
    /// max already grows on its own every frame (`HudInfo::sat_cap`), so this
    /// toast is the only piece that needs an explicit trigger.
    pub fn show_levelup_toast(&mut self, level: u64, sat_cap: u8, current_hue: u16) {
        self.toast = Some(Toast {
            text: format!("Level up! Lv{level} — saturation cap now {sat_cap}%"),
            hue: Some(current_hue),
            shown_at: Instant::now(),
        });
    }

    /// F9.6 item 2: plain-text toast (no flash swatch) — used for the
    /// middle-click eyedropper's "not unlocked" feedback.
    pub fn show_info_toast(&mut self, text: String) {
        self.toast = Some(Toast { text, hue: None, shown_at: Instant::now() });
    }

    /// Seeds the name field from the server row exactly once. After that the
    /// field belongs to local editing — the subscription echoing our own
    /// `set_name` call back must not clobber in-progress typing.
    pub fn sync_name_once(&mut self, server_name: Option<&String>) {
        if !self.name_loaded {
            self.name_input = server_name.cloned().unwrap_or_default();
            self.name_loaded = true;
        }
    }

    /// Most-recently-used first, deduped, capped at 3.
    pub fn note_used_hue(&mut self, hue: u16) {
        self.last3.retain(|&h| h != hue);
        self.last3.push_front(hue);
        self.last3.truncate(3);
    }

    /// F9.5 item 4 (author-caught, `known_bugs.md`: "the selected color is
    /// not in the recent color used on launch"): seed the footer's last-3
    /// ring with the caller's own starting/current hue the first time it's
    /// known, so a brand-new connection shows something there instead of
    /// staying empty until the player's first merge or swatch click. A no-op
    /// once `last3` has any entry, so the caller can just call this every
    /// frame after `me` becomes known rather than tracking its own
    /// seed-once flag.
    pub fn seed_last3_once(&mut self, hue: u16) {
        if self.last3.is_empty() {
            self.last3.push_front(hue);
        }
    }

    /// F9.5 item 4 (author-caught: "reset account doesn't reset the last 3
    /// selected colors"): `reset_account` wipes the caller's entire
    /// inventory down to one fresh hue, but `note_used_hue` alone would just
    /// prepend that hue onto the EXISTING ring, leaving up to two
    /// now-meaningless pre-reset colors still showing. Clears first.
    pub fn note_reset_hue(&mut self, hue: u16) {
        self.last3.clear();
        self.last3.push_front(hue);
    }
}

/// Snapshot of server-derived state the HUD needs to read this frame.
/// Passed to both `handle_input` (for cap/clamp logic) and `draw`.
pub struct HudInfo<'a> {
    /// Short (8-hex-char) identity label for the header — precomputed by
    /// the caller, whose identity type differs between the native (SDK
    /// `Identity`) and web (JSON hex string) clients. Keeping SDK types out
    /// of this module's public interface is what lets `bin/web.rs` reuse it.
    pub short_id: &'a str,
    pub level: u64,
    pub xp: u64,
    pub online: usize,
    pub total: usize,
    pub locked: bool,
    pub brush: (u16, u8, u8),
    pub sat_cap: u8,
    pub hues: &'a [u16],
    /// Whether to render the paste-token import field in the Account
    /// overlay. True on web (the judged target, per plan.md F6); false on
    /// native, where reconnecting as an imported identity would need a full
    /// process restart and is explicitly out of scope for the jam.
    pub show_token_import: bool,
    /// Seconds remaining until the F8 re-rank fires, if `config.next_rerank_at`
    /// is set and still in the future — drives the countdown banner.
    pub rerank_secs: Option<i64>,
}

#[derive(Default)]
pub struct Actions {
    pub set_brush: Option<(u16, u8, u8)>,
    pub set_name: Option<String>,
    pub set_lock: Option<bool>,
    pub center_camera: bool,
    /// Copy the full reconnection token (not just the header's short hex) to
    /// the clipboard.
    pub copy_token: bool,
    /// A token pasted into the Account overlay's import field, submitted via
    /// the Import button or Enter.
    pub import_token: Option<String>,
    pub reset_account: bool,
    /// F8: like the island in the currently-open island-info popup.
    pub like_island: Option<u32>,
    /// Author-requested: undo a like from the popup's now-toggling button.
    pub unlike_island: Option<u32>,
    /// Footer button: open the caller's own island-info popup.
    pub open_own_island: bool,
    /// F9: set/replace the caller's own island's itch.io rate id, from the
    /// popup's link edit field.
    pub set_island_link: Option<u32>,
    /// F9: a foreign island's link row was clicked — `(island_id, rate_id)`.
    /// The caller opens the URL AND fires `click_link` for XP; both use the
    /// same click, see the popup's link-row hit test.
    pub click_link: Option<(u32, u32)>,
}

fn footer_bg() -> Rectangle {
    Rectangle::new(0.0, SCREEN_H - FOOTER_H, SCREEN_W, FOOTER_H)
}

fn center_btn_rect() -> Rectangle {
    Rectangle::new(8.0, SCREEN_H - FOOTER_H + 7.0, 54.0, 30.0)
}

fn last3_rect(i: usize) -> Rectangle {
    Rectangle::new(70.0 + i as f32 * 34.0, SCREEN_H - FOOTER_H + 7.0, 30.0, 30.0)
}

fn inventory_btn_rect() -> Rectangle {
    Rectangle::new(174.0, SCREEN_H - FOOTER_H + 7.0, 64.0, 30.0)
}

/// Author-requested: icon-only (see `draw_footer`), sitting directly left of
/// the name field.
fn eraser_btn_rect() -> Rectangle {
    Rectangle::new(246.0, SCREEN_H - FOOTER_H + 7.0, 30.0, 30.0)
}

/// Author-requested: icon-only (see `draw_footer`), left of the name field,
/// right next to the eraser toggle.
fn lock_btn_rect() -> Rectangle {
    Rectangle::new(280.0, SCREEN_H - FOOTER_H + 7.0, 30.0, 30.0)
}

/// Author-requested: narrower than before now that the eraser/lock icons
/// moved to its left.
fn name_field_rect() -> Rectangle {
    Rectangle::new(318.0, SCREEN_H - FOOTER_H + 7.0, 150.0, 30.0)
}

fn account_btn_rect() -> Rectangle {
    Rectangle::new(478.0, SCREEN_H - FOOTER_H + 7.0, 80.0, 30.0)
}

/// Author-requested: a footer button to open the caller's own island-info
/// popup, replacing the old "click your own island" gesture (which just
/// painted the cell it was released on, so the popup never actually showed).
fn my_island_btn_rect() -> Rectangle {
    Rectangle::new(566.0, SCREEN_H - FOOTER_H + 7.0, 72.0, 30.0)
}

fn overlay_rect() -> Rectangle {
    Rectangle::new(60.0, 60.0, 600.0, 560.0)
}

fn overlay_close_rect() -> Rectangle {
    let o = overlay_rect();
    Rectangle::new(o.x + o.width - 38.0, o.y + 8.0, 30.0, 30.0)
}

const SWATCH: f32 = 44.0;
const SWATCH_GAP: f32 = 8.0;
const SWATCH_COLS: usize = 10;

fn swatch_rect(i: usize) -> Rectangle {
    let o = overlay_rect();
    let col = (i % SWATCH_COLS) as f32;
    let row = (i / SWATCH_COLS) as f32;
    Rectangle::new(
        o.x + 20.0 + col * (SWATCH + SWATCH_GAP),
        o.y + 50.0 + row * (SWATCH + SWATCH_GAP),
        SWATCH,
        SWATCH,
    )
}

fn hue_slider_rect() -> Rectangle {
    let o = overlay_rect();
    Rectangle::new(o.x + 40.0, o.y + o.height - 160.0, 440.0, 16.0)
}

fn sat_slider_rect() -> Rectangle {
    let o = overlay_rect();
    Rectangle::new(o.x + 40.0, o.y + o.height - 110.0, 440.0, 16.0)
}

fn val_slider_rect() -> Rectangle {
    let o = overlay_rect();
    Rectangle::new(o.x + 40.0, o.y + o.height - 60.0, 440.0, 16.0)
}

fn copy_btn_rect() -> Rectangle {
    let o = overlay_rect();
    Rectangle::new(o.x + 20.0, o.y + 60.0, 180.0, 36.0)
}

fn import_field_rect() -> Rectangle {
    let o = overlay_rect();
    Rectangle::new(o.x + 20.0, o.y + 180.0, 320.0, 32.0)
}

fn import_btn_rect() -> Rectangle {
    let o = overlay_rect();
    Rectangle::new(o.x + 350.0, o.y + 180.0, 100.0, 32.0)
}

fn reset_btn_rect() -> Rectangle {
    let o = overlay_rect();
    Rectangle::new(o.x + 20.0, o.y + 280.0, 240.0, 36.0)
}

/// F9: own-island popup only — numeric input for the itch.io rate id.
fn link_edit_rect() -> Rectangle {
    let o = overlay_rect();
    Rectangle::new(o.x + 20.0, o.y + 180.0, 200.0, 32.0)
}

fn link_set_btn_rect() -> Rectangle {
    let o = overlay_rect();
    Rectangle::new(o.x + 230.0, o.y + 180.0, 100.0, 32.0)
}

fn rerank_banner_rect() -> Rectangle {
    Rectangle::new(210.0, SCREEN_H - FOOTER_H - 32.0, 300.0, 24.0)
}

/// `mouse_x` -> integer value in `0..=max`, clamped to the track's extent.
fn slider_value(track: Rectangle, mouse_x: f32, max: f32) -> u8 {
    (((mouse_x - track.x) / track.width).clamp(0.0, 1.0) * max).round() as u8
}

/// Vertically-padded hit box so a slider is easy to grab, not just its
/// visual 16px track.
fn slider_hit(track: Rectangle) -> Rectangle {
    Rectangle::new(track.x, track.y - 10.0, track.width, track.height + 20.0)
}

/// Every swatch (footer last-3, inventory grid) is rendered at the CURRENT
/// brush sat/val, not just the selected hue — dragging the sliders previews
/// what every unlocked hue would look like at that sat/val, which is the
/// whole point of comparing them side by side before picking one.
fn swatch_color(hue: u16, info: &HudInfo) -> Color {
    world::hsv_color(hue, info.brush.1, info.brush.2)
}

/// The brush hue to treat as "current" for Hue-slider bookkeeping: an
/// explicit click's own just-requested hue until the server confirms it via
/// `info.brush.0`, which avoids a 1-frame mismatch right after the click
/// (see `UiState::pending_select`).
fn effective_hue(state: &UiState, info: &HudInfo) -> u16 {
    state.pending_select.unwrap_or(info.brush.0)
}

/// F9.6 item 4: `info.hues` reflects DB iteration order (effectively
/// insertion order); the inventory grid instead shows them sorted by hue so
/// nearby colors sit next to each other. Both `handle_input`'s swatch click
/// loop and `draw_overlay` call this so index `i` -> `swatch_rect(i)` always
/// means the same hue in both places.
fn sorted_hues(info: &HudInfo) -> Vec<u16> {
    let mut hues = info.hues.to_vec();
    hues.sort_unstable();
    hues
}

/// F9.6 item 4: swatch hex code, as actually rendered (current brush
/// sat/val, not some canonical 100/100) — mirrors `swatch_color`.
fn color_hex(c: Color) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}

pub fn handle_input(rl: &mut RaylibHandle, state: &mut UiState, info: &HudInfo) -> Actions {
    if state.toast.as_ref().is_some_and(|t| t.shown_at.elapsed() >= TOAST_DURATION) {
        state.toast = None;
    }
    state.like_anims.retain(|a| a.started_at.elapsed() < LIKE_ANIM_DURATION);
    if state.pending_select == Some(info.brush.0) {
        state.pending_select = None;
    }
    if state.reset_armed_at.is_some_and(|t| t.elapsed() >= RESET_CONFIRM_WINDOW) {
        state.reset_armed_at = None;
    }
    if state.copy_clicked_at.is_some_and(|t| t.elapsed() >= TOAST_DURATION) {
        state.copy_clicked_at = None;
    }
    // Re-anchor the Hue slider whenever the actual brush hue has drifted
    // outside its window (a merge changed it server-side, the live hue was
    // left nudged away from any exact swatch by a previous session, or this
    // is the very first frame and `base_hue` is still its `0` default) — run
    // before any click handling below so an explicit swatch click this same
    // frame (which sets `base_hue` directly) always wins over this. Snaps to
    // the NEAREST owned exact hue rather than the raw (possibly nudged)
    // brush value, so the inventory grid's "selected" swatch is always some
    // real entry, never a value that matches nothing in `info.hues`.
    let cur_hue = effective_hue(state, info);
    if world::hue_dist(cur_hue, state.base_hue) > world::constants::HUE_TOLERANCE {
        state.base_hue = info
            .hues
            .iter()
            .copied()
            .min_by_key(|&h| world::hue_dist(cur_hue, h))
            .unwrap_or(cur_hue);
    }
    let mut actions = Actions::default();
    let mouse = rl.get_mouse_position();
    let clicked = rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT);
    let held = rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT);
    let released = rl.is_mouse_button_released(MouseButton::MOUSE_BUTTON_LEFT);

    // F9.6 item 5: Escape closes whatever single overlay is open (item 4's
    // rule, applied uniformly to all four); with nothing open, it toggles
    // the help overlay instead. Checked first and returns immediately so a
    // frame that opens/closes an overlay doesn't also fall through to the
    // click handling below.
    if rl.is_key_pressed(KeyboardKey::KEY_ESCAPE) {
        if state.help_open {
            state.help_open = false;
        } else if state.overlay_open {
            state.overlay_open = false;
            state.dragging = Drag::None;
        } else if state.account_open {
            state.account_open = false;
        } else if state.island_popup.is_some() {
            state.island_popup = None;
        } else {
            state.help_open = true;
        }
        return actions;
    }
    // F9.6 item 1: eraser toggle — `X` (not `E`, which item 6 gives to
    // keyboard zoom-in), swallowed while a text field owns keyboard input so
    // typing a name containing "x" doesn't flip it.
    if rl.is_key_pressed(KeyboardKey::KEY_X) && !state.text_field_focused() {
        state.eraser_on = !state.eraser_on;
    }
    if clicked && point_in(mouse, eraser_btn_rect()) {
        state.eraser_on = !state.eraser_on;
    }

    if clicked && point_in(mouse, center_btn_rect()) {
        actions.center_camera = true;
    }
    // Collected first, applied after: `note_used_hue` below needs `&mut
    // state.last3`, which can't happen while this loop still holds `.iter()`
    // borrowed from it.
    let mut last3_clicked = None;
    for (i, &hue) in state.last3.iter().enumerate() {
        if clicked && point_in(mouse, last3_rect(i)) {
            last3_clicked = Some(hue);
        }
    }
    if let Some(hue) = last3_clicked {
        actions.set_brush = Some((hue, info.brush.1.min(info.sat_cap), info.brush.2));
        state.base_hue = hue;
        state.pending_select = Some(hue);
        state.note_used_hue(hue);
    }
    if clicked && point_in(mouse, inventory_btn_rect()) {
        state.overlay_open = !state.overlay_open;
        if state.overlay_open {
            state.account_open = false;
            state.island_popup = None;
            state.help_open = false;
        }
        // Must return here: the footer button sits below `overlay_rect()`,
        // so without this the "click outside the panel closes it" check
        // further down would see this same click, land outside the panel,
        // and immediately close the overlay it just opened.
        return actions;
    }
    if clicked && point_in(mouse, lock_btn_rect()) {
        actions.set_lock = Some(!info.locked);
    }
    if clicked && point_in(mouse, account_btn_rect()) {
        state.account_open = !state.account_open;
        if state.account_open {
            state.overlay_open = false;
            state.island_popup = None;
            state.help_open = false;
        }
    }
    if clicked && point_in(mouse, my_island_btn_rect()) {
        actions.open_own_island = true;
        state.overlay_open = false;
        state.account_open = false;
        state.help_open = false;
    }

    // Name field: click to focus/blur (blur commits), Enter commits+blurs.
    let name_rect = name_field_rect();
    if clicked {
        let now_over = point_in(mouse, name_rect);
        if state.name_focused && !now_over {
            state.name_focused = false;
            actions.set_name = Some(state.name_input.clone());
        } else if now_over {
            state.name_focused = true;
        }
    }
    if state.name_focused {
        while let Some(c) = rl.get_char_pressed() {
            if !c.is_control() && state.name_input.chars().count() < 20 {
                state.name_input.push(c);
            }
        }
        if rl.is_key_pressed(KeyboardKey::KEY_BACKSPACE) {
            state.name_input.pop();
        }
        if rl.is_key_pressed(KeyboardKey::KEY_ENTER) {
            state.name_focused = false;
            actions.set_name = Some(state.name_input.clone());
        }
    }

    // F9.6 item 5: help overlay is modal too — closes on its X button or an
    // outside click (matching item 4's rule for the inventory overlay),
    // besides the Escape toggle handled at the top of this function.
    if state.help_open {
        if clicked && (point_in(mouse, overlay_close_rect()) || !point_in(mouse, overlay_rect())) {
            state.help_open = false;
        }
        return actions;
    }

    // Account overlay is modal too, and mutually exclusive with the
    // inventory overlay (only one can be open, enforced by the toggles
    // above) — handled and returned here before the inventory-overlay gate.
    if state.account_open {
        if clicked && point_in(mouse, overlay_close_rect()) {
            state.account_open = false;
            return actions;
        }
        if clicked && point_in(mouse, copy_btn_rect()) {
            actions.copy_token = true;
            state.copy_clicked_at = Some(Instant::now());
        }
        if info.show_token_import {
            let field = import_field_rect();
            if clicked {
                state.import_focused = point_in(mouse, field);
            }
            if state.import_focused {
                while let Some(c) = rl.get_char_pressed() {
                    if !c.is_control() && state.import_input.chars().count() < IMPORT_TOKEN_MAX_LEN {
                        state.import_input.push(c);
                    }
                }
                if rl.is_key_pressed(KeyboardKey::KEY_BACKSPACE) {
                    state.import_input.pop();
                }
                // A pasted token is ~200+ chars — typing it by hand isn't a
                // real option, so Ctrl+V/Cmd+V must work here, unlike the
                // name field above (short enough to type).
                let pasting = rl.is_key_pressed(KeyboardKey::KEY_V)
                    && (rl.is_key_down(KeyboardKey::KEY_LEFT_CONTROL)
                        || rl.is_key_down(KeyboardKey::KEY_RIGHT_CONTROL)
                        || rl.is_key_down(KeyboardKey::KEY_LEFT_SUPER)
                        || rl.is_key_down(KeyboardKey::KEY_RIGHT_SUPER));
                if pasting {
                    if let Ok(clip) = rl.get_clipboard_text() {
                        let room = IMPORT_TOKEN_MAX_LEN.saturating_sub(state.import_input.chars().count());
                        state.import_input.extend(clip.trim().chars().take(room));
                    }
                }
            }
            let submit = (clicked && point_in(mouse, import_btn_rect()))
                || (state.import_focused && rl.is_key_pressed(KeyboardKey::KEY_ENTER));
            if submit && !state.import_input.trim().is_empty() {
                actions.import_token = Some(state.import_input.trim().to_string());
                state.import_input.clear();
                state.import_focused = false;
            }
        }
        if clicked && point_in(mouse, reset_btn_rect()) {
            if state.reset_armed_at.is_some() {
                actions.reset_account = true;
                state.reset_armed_at = None;
            } else {
                state.reset_armed_at = Some(Instant::now());
            }
        }
        return actions;
    }

    // F8/F9.5 island-info: two very different UIs share `island_popup`.
    // Own island (via the "My Isle" footer button, a deliberate action) is
    // still the full modal panel below, with a close button and the link
    // editor. A FOREIGN island's popup (F9.5 item 7: opened by hovering) is
    // a small, non-interactive tooltip drawn by `draw_island_tooltip` —
    // no close button (it closes itself on hover-out, in
    // `main.rs`/`bin/web.rs`) and no click handling here at all;
    // double-click-to-like and a direct single-click-to-open-link both live
    // in the map-click code instead, since a box that continuously re-glues
    // itself to the mouse can't contain a clickable target you could ever
    // actually reach.
    if let Some(popup) = &state.island_popup {
        if popup.is_own {
            if clicked && point_in(mouse, overlay_close_rect()) {
                state.island_popup = None;
                return actions;
            }
            // F9: own-island link editing — digits only (it's a numeric
            // itch.io submission id), same Ctrl+V-friendly typing as the
            // Account overlay's token import field.
            let field = link_edit_rect();
            if clicked {
                state.link_edit_focused = point_in(mouse, field);
            }
            if state.link_edit_focused {
                while let Some(c) = rl.get_char_pressed() {
                    if c.is_ascii_digit() && state.link_edit_input.len() < 10 {
                        state.link_edit_input.push(c);
                    }
                }
                if rl.is_key_pressed(KeyboardKey::KEY_BACKSPACE) {
                    state.link_edit_input.pop();
                }
            }
            let submit = (clicked && point_in(mouse, link_set_btn_rect()))
                || (state.link_edit_focused && rl.is_key_pressed(KeyboardKey::KEY_ENTER));
            if submit {
                if let Ok(id) = state.link_edit_input.trim().parse::<u32>() {
                    actions.set_island_link = Some(id);
                }
            }
        }
        return actions;
    }

    if !state.overlay_open {
        state.dragging = Drag::None;
        return actions;
    }

    // Overlay is modal: swallow all remaining input here so the map behind
    // it never sees clicks/drags while it's open (caller still checks
    // `state.overlay_open` before touching camera/paint input).
    if clicked && point_in(mouse, overlay_close_rect()) {
        state.overlay_open = false;
        return actions;
    }
    // F9.6 item 4: clicking anywhere outside the panel closes it too (not
    // just the X button) — everything interactive inside the panel (close
    // button, swatches, sliders) is checked individually below/above, so a
    // click that matched none of them landed on the dimmed backdrop.
    if clicked && !point_in(mouse, overlay_rect()) {
        state.overlay_open = false;
        state.dragging = Drag::None;
        return actions;
    }
    // F9.6 item 4: swatches sorted by hue — `draw_overlay` iterates the same
    // sorted order so swatch indices (and thus `swatch_rect(i)`) line up
    // between the two.
    for (i, &hue) in sorted_hues(info).iter().enumerate() {
        if clicked && point_in(mouse, swatch_rect(i)) {
            actions.set_brush = Some((hue, info.brush.1.min(info.sat_cap), info.brush.2));
            state.base_hue = hue;
            state.pending_select = Some(hue);
            state.note_used_hue(hue);
        }
    }

    let hue_hit = slider_hit(hue_slider_rect());
    let sat_hit = slider_hit(sat_slider_rect());
    let val_hit = slider_hit(val_slider_rect());
    if clicked {
        if point_in(mouse, hue_hit) {
            state.dragging = Drag::Hue;
        } else if point_in(mouse, sat_hit) {
            state.dragging = Drag::Sat;
        } else if point_in(mouse, val_hit) {
            state.dragging = Drag::Val;
        }
    }
    if released {
        state.dragging = Drag::None;
    }
    if held {
        match state.dragging {
            Drag::Hue => {
                let tol = world::constants::HUE_TOLERANCE;
                let track = hue_slider_rect();
                let frac = ((mouse.x - track.x) / track.width).clamp(0.0, 1.0);
                let offset = (frac * (2 * tol) as f32).round() as i32 - tol;
                let target = (state.base_hue as i32 + offset).rem_euclid(360) as u16;
                if target != info.brush.0 {
                    actions.set_brush = Some((target, info.brush.1, info.brush.2));
                }
            }
            Drag::Sat => {
                let v = slider_value(sat_slider_rect(), mouse.x, info.sat_cap as f32);
                if v != info.brush.1 {
                    actions.set_brush = Some((info.brush.0, v, info.brush.2));
                }
            }
            Drag::Val => {
                let v = slider_value(val_slider_rect(), mouse.x, 100.0);
                if v != info.brush.2 {
                    actions.set_brush = Some((info.brush.0, info.brush.1, v));
                }
            }
            Drag::None => {}
        }
    }

    actions
}

pub fn draw(d: &mut impl RaylibDraw, state: &UiState, info: &HudInfo, mouse: Vector2) {
    draw_header(d, info);
    draw_footer(d, state, info);
    if state.overlay_open {
        draw_overlay(d, state, info, mouse);
    } else if state.account_open {
        draw_account_overlay(d, state, info);
    } else if let Some(popup) = &state.island_popup {
        if popup.is_own {
            draw_island_popup(d, state, popup);
        } else {
            draw_island_tooltip(d, popup, mouse);
        }
    } else if state.help_open {
        draw_help_overlay(d);
    }
    if let Some(toast) = &state.toast {
        draw_toast(d, toast, info);
    }
    if let Some(secs) = info.rerank_secs {
        draw_rerank_banner(d, secs);
    }
    draw_like_anims(d, state);
}

/// F8/F9: own-island management panel (opened via the "My Isle" footer
/// button — a deliberate action, unlike the hover tooltip below) — creator
/// line, likes, age, and the link edit field/button. Full modal treatment
/// (backdrop, close button) since it has real form controls to interact
/// with, unlike the foreign-island case.
fn draw_island_popup(d: &mut impl RaylibDraw, state: &UiState, popup: &IslandInfo) {
    d.draw_rectangle_rec(Rectangle::new(0.0, 0.0, SCREEN_W, SCREEN_H), Color::new(0, 0, 0, 140));

    let o = overlay_rect();
    d.draw_rectangle_rec(o, Color::new(24, 24, 30, 250));
    d.draw_rectangle_lines_ex(o, 2.0, Color::new(90, 90, 100, 255));
    d.draw_text("Island", o.x as i32 + 20, o.y as i32 + 14, 18, Color::RAYWHITE);

    let close = overlay_close_rect();
    d.draw_rectangle_rec(close, Color::new(60, 40, 40, 255));
    d.draw_text("X", close.x as i32 + 11, close.y as i32 + 7, 16, Color::RAYWHITE);

    d.draw_text(&format!("Owner: {}", popup.owner_label), o.x as i32 + 20, o.y as i32 + 54, 16, Color::RAYWHITE);
    // F9.6 item 3: heart glyph in place of the old text-only "Likes: N" —
    // filled when the viewer (the popup only ever opens on your OWN island,
    // so this is always `false` here in practice, kept for symmetry with
    // the tooltip below which does need it) has already liked it.
    let heart_color = if popup.already_liked { Color::new(230, 70, 90, 255) } else { Color::new(160, 160, 168, 255) };
    world::draw_heart(d, Vector2::new(o.x + 30.0, o.y + 86.0), 18.0, popup.already_liked, heart_color);
    d.draw_text(&format!("{}", popup.likes), o.x as i32 + 44, o.y as i32 + 78, 16, Color::RAYWHITE);
    d.draw_text(&format!("Created {}", popup.age_label), o.x as i32 + 20, o.y as i32 + 106, 16, Color::LIGHTGRAY);

    let link_label = match popup.link_id {
        Some(id) => format!("Your link: itch.io rate #{id}"),
        None => "Your link: not set".to_string(),
    };
    d.draw_text(&link_label, o.x as i32 + 20, o.y as i32 + 132, 16, Color::LIGHTGRAY);
    d.draw_text("Set your itch.io rate id:", o.x as i32 + 20, o.y as i32 + 162, 14, Color::LIGHTGRAY);

    let field = link_edit_rect();
    d.draw_rectangle_rec(field, Color::new(28, 28, 34, 255));
    d.draw_rectangle_lines_ex(field, 1.0, if state.link_edit_focused { Color::GOLD } else { Color::new(90, 90, 96, 255) });
    let shown = if state.link_edit_input.is_empty() && !state.link_edit_focused { "e.g. 123456" } else { &state.link_edit_input };
    d.draw_text(shown, field.x as i32 + 6, field.y as i32 + 7, 14, Color::RAYWHITE);

    let sb = link_set_btn_rect();
    d.draw_rectangle_rec(sb, Color::new(40, 40, 48, 255));
    d.draw_text("Set", sb.x as i32 + 34, sb.y as i32 + 9, 14, Color::RAYWHITE);
}

const TOOLTIP_W: f32 = 220.0;
const TOOLTIP_PAD: f32 = 8.0;
const TOOLTIP_LINE_H: f32 = 18.0;

/// F9.5 item 7 (author-requested redesign): a foreign island's info, as a
/// small, non-interactive tooltip glued to the cursor (offset so it doesn't
/// sit under it) instead of F8's big centered modal — no backdrop dim, no
/// close button (the caller closes it automatically on hover-out), no
/// buttons at all: a box that continuously re-centers on the mouse can
/// never contain a clickable target you could actually reach, since moving
/// toward it moves it the same distance. Like/unlike stays exclusively a
/// double-click on the map; opening the link is a direct single click on
/// the island itself — see the map-click code in `main.rs`/`bin/web.rs`.
fn draw_island_tooltip(d: &mut impl RaylibDraw, popup: &IslandInfo, mouse: Vector2) {
    let lines = if popup.link_id.is_some() { 4 } else { 3 };
    let height = TOOLTIP_PAD * 2.0 + TOOLTIP_LINE_H * lines as f32;
    let mut x = mouse.x + 18.0;
    let mut y = mouse.y + 18.0;
    if x + TOOLTIP_W > SCREEN_W {
        x = mouse.x - TOOLTIP_W - 12.0;
    }
    if y + height > SCREEN_H {
        y = mouse.y - height - 12.0;
    }
    let rect = Rectangle::new(x, y, TOOLTIP_W, height);
    d.draw_rectangle_rec(rect, Color::new(20, 20, 26, 235));
    d.draw_rectangle_lines_ex(rect, 1.0, Color::new(120, 120, 130, 200));

    let tx = rect.x as i32 + TOOLTIP_PAD as i32;
    let mut ty = rect.y as i32 + TOOLTIP_PAD as i32;
    d.draw_text(&popup.owner_label, tx, ty, 15, Color::RAYWHITE);
    ty += TOOLTIP_LINE_H as i32;
    // F9.6 item 3: heart glyph (filled = you've already liked this island)
    // instead of the old text-only "Likes: N".
    let heart_color = if popup.already_liked { Color::new(230, 70, 90, 255) } else { Color::new(160, 160, 168, 255) };
    world::draw_heart(d, Vector2::new(tx as f32 + 8.0, ty as f32 + 7.0), 14.0, popup.already_liked, heart_color);
    d.draw_text(&format!("{}", popup.likes), tx + 20, ty, 13, Color::LIGHTGRAY);
    ty += TOOLTIP_LINE_H as i32;
    d.draw_text(&popup.age_label, tx, ty, 12, Color::GRAY);
    if let Some(id) = popup.link_id {
        ty += TOOLTIP_LINE_H as i32;
        d.draw_text(&format!("Linked: rate #{id}"), tx, ty, 12, Color::new(120, 180, 255, 255));
    }
}

/// Author-requested: a floating "+1"/"-1" pop where a double-click
/// like/unlike landed, since that gesture (unlike the popup button) has no
/// other visible feedback. Grows slightly and fades out over
/// `LIKE_ANIM_DURATION`; screen-space, drawn over everything else.
fn draw_like_anims(d: &mut impl RaylibDraw, state: &UiState) {
    for anim in &state.like_anims {
        let t = (anim.started_at.elapsed().as_secs_f32() / LIKE_ANIM_DURATION.as_secs_f32()).clamp(0.0, 1.0);
        let alpha = ((1.0 - t) * 255.0) as u8;
        let rise = t * 26.0;
        let radius = 10.0 + t * 6.0;
        let cy = anim.pos.y - rise;
        let ring = if anim.liked { Color::new(230, 70, 90, alpha) } else { Color::new(160, 160, 168, alpha) };
        d.draw_circle(anim.pos.x as i32, cy as i32, radius, ring);
        let label = if anim.liked { "+1" } else { "-1" };
        d.draw_text(label, anim.pos.x as i32 - 8, cy as i32 - 8, 16, Color::new(255, 255, 255, alpha));
    }
}

/// Bottom-of-screen banner (own spot, away from the header/toast area) while
/// `config.next_rerank_at` counts down to an F8 re-rank.
fn draw_rerank_banner(d: &mut impl RaylibDraw, secs: i64) {
    let bar = rerank_banner_rect();
    d.draw_rectangle_rec(bar, Color::new(24, 24, 30, 235));
    d.draw_rectangle_lines_ex(bar, 1.0, Color::new(255, 215, 0, 220));
    d.draw_text(
        &format!("Islands re-ranking in {secs}s"),
        bar.x as i32 + 10,
        bar.y as i32 + 5,
        14,
        Color::RAYWHITE,
    );
}

/// Banner just under the header, with a flash swatch that fades out over
/// `TOAST_DURATION` (the "new color" feedback from a merge).
fn draw_toast(d: &mut impl RaylibDraw, toast: &Toast, info: &HudInfo) {
    let frac = 1.0 - (toast.shown_at.elapsed().as_secs_f32() / TOAST_DURATION.as_secs_f32()).clamp(0.0, 1.0);
    let alpha = (frac * 235.0) as u8;
    let bar = Rectangle::new(180.0, HEADER_H + 10.0, 360.0, 34.0);
    d.draw_rectangle_rec(bar, Color::new(24, 24, 30, alpha));
    d.draw_rectangle_lines_ex(bar, 1.0, Color::new(255, 215, 0, alpha));
    let text_x = if let Some(hue) = toast.hue {
        let swatch = Rectangle::new(bar.x + 6.0, bar.y + 6.0, 22.0, 22.0);
        let mut flash = world::hsv_color(hue, info.sat_cap, 90);
        flash.a = alpha;
        d.draw_rectangle_rec(swatch, flash);
        bar.x as i32 + 36
    } else {
        bar.x as i32 + 10
    };
    d.draw_text(&toast.text, text_x, bar.y as i32 + 9, 14, Color::new(255, 255, 255, alpha));
}

fn draw_header(d: &mut impl RaylibDraw, info: &HudInfo) {
    d.draw_rectangle_rec(Rectangle::new(0.0, 0.0, SCREEN_W, HEADER_H), Color::new(10, 10, 14, 235));
    d.draw_text(
        &format!("{}   Lv{}  {}xp", info.short_id, info.level, info.xp),
        10,
        6,
        16,
        Color::RAYWHITE,
    );
    // Fixed-position right-side label rather than measuring text width —
    // the draw handle has no default-font `measure_text` (that's only on
    // `RaylibHandle`, unavailable once `begin_drawing` hands out its borrow).
    d.draw_text(
        &format!("{} / {} online", info.online, info.total),
        560,
        6,
        16,
        Color::LIGHTGRAY,
    );
}

fn draw_footer(d: &mut impl RaylibDraw, state: &UiState, info: &HudInfo) {
    d.draw_rectangle_rec(footer_bg(), Color::new(10, 10, 14, 235));

    let cb = center_btn_rect();
    d.draw_rectangle_rec(cb, Color::new(40, 40, 48, 255));
    d.draw_text("Center", cb.x as i32 + 9, cb.y as i32 + 9, 10, Color::RAYWHITE);

    for (i, &hue) in state.last3.iter().enumerate() {
        let r = last3_rect(i);
        d.draw_rectangle_rec(r, swatch_color(hue, info));
        d.draw_rectangle_lines_ex(r, 1.0, Color::new(200, 200, 200, 180));
    }

    let ib = inventory_btn_rect();
    d.draw_rectangle_rec(ib, Color::new(40, 40, 48, 255));
    d.draw_text("Colors", ib.x as i32 + 6, ib.y as i32 + 7, 14, Color::RAYWHITE);

    // Author-requested: icon-only, left of the name field. The icon itself
    // now shows which TOOL is active (pencil = painting, eraser = erasing)
    // rather than a static "eraser" glyph that only ever meant "click to
    // erase" — a red border is the active-state signal instead of a solid
    // fill, so it reads apart from the Lock button's fill-based signal
    // right next to it.
    let eb = eraser_btn_rect();
    d.draw_rectangle_rec(eb, Color::new(40, 40, 48, 255));
    if state.eraser_on {
        d.draw_rectangle_lines_ex(eb, 2.0, Color::new(220, 70, 70, 255));
        draw_eraser_icon(d, eb);
    } else {
        draw_pencil_icon(d, eb);
    }

    let lb = lock_btn_rect();
    d.draw_rectangle_rec(lb, if info.locked { Color::new(120, 60, 60, 255) } else { Color::new(40, 40, 48, 255) });
    draw_lock_icon(d, lb, info.locked);

    let nf = name_field_rect();
    d.draw_rectangle_rec(nf, Color::new(28, 28, 34, 255));
    d.draw_rectangle_lines_ex(nf, 1.0, if state.name_focused { Color::GOLD } else { Color::new(90, 90, 96, 255) });
    let label = if state.name_input.is_empty() && !state.name_focused { "name..." } else { &state.name_input };
    d.draw_text(label, nf.x as i32 + 6, nf.y as i32 + 7, 14, Color::RAYWHITE);

    let ab = account_btn_rect();
    d.draw_rectangle_rec(ab, Color::new(40, 40, 48, 255));
    d.draw_text("Account", ab.x as i32 + 8, ab.y as i32 + 9, 10, Color::RAYWHITE);

    let mb = my_island_btn_rect();
    d.draw_rectangle_rec(mb, Color::new(40, 40, 48, 255));
    d.draw_text("My Isle", mb.x as i32 + 6, mb.y as i32 + 9, 10, Color::RAYWHITE);
}

/// Rotates `p` around `center` by `angle_rad` (screen-space, y-down).
fn rotate_around(p: Vector2, center: Vector2, angle_rad: f32) -> Vector2 {
    let (s, c) = angle_rad.sin_cos();
    let dx = p.x - center.x;
    let dy = p.y - center.y;
    Vector2::new(center.x + dx * c - dy * s, center.y + dx * s + dy * c)
}

/// Fills a convex quad (corners given in order) as two triangles — raylib
/// has no rotated-rectangle primitive that keeps this file's "primitives
/// only" rule, so a rotated rect is just two `draw_triangle` calls.
fn draw_quad(d: &mut impl RaylibDraw, p0: Vector2, p1: Vector2, p2: Vector2, p3: Vector2, color: Color) {
    d.draw_triangle(p0, p1, p2, color);
    d.draw_triangle(p0, p2, p3, color);
}

/// Author-requested: default (paint-mode) icon for the paint/erase toggle —
/// a diagonal pencil with a pink eraser cap and a dark tip, swapped for
/// `draw_eraser_icon` while `state.eraser_on` is true (see `draw_footer`),
/// so the button always shows which tool is currently active.
fn draw_pencil_icon(d: &mut impl RaylibDraw, r: Rectangle) {
    let center = Vector2::new(r.x + r.width / 2.0, r.y + r.height / 2.0);
    let angle = 45f32.to_radians();
    let half_w = 3.0;
    let cap_top = center.y - 12.0;
    let cap_bottom = center.y - 6.0;
    let body_bottom = center.y + 7.0;
    let tip = center.y + 12.0;
    let left = center.x - half_w;
    let right = center.x + half_w;

    let cap = [
        rotate_around(Vector2::new(left, cap_top), center, angle),
        rotate_around(Vector2::new(right, cap_top), center, angle),
        rotate_around(Vector2::new(right, cap_bottom), center, angle),
        rotate_around(Vector2::new(left, cap_bottom), center, angle),
    ];
    draw_quad(d, cap[0], cap[1], cap[2], cap[3], Color::new(235, 120, 150, 255));

    let body = [
        rotate_around(Vector2::new(left, cap_bottom), center, angle),
        rotate_around(Vector2::new(right, cap_bottom), center, angle),
        rotate_around(Vector2::new(right, body_bottom), center, angle),
        rotate_around(Vector2::new(left, body_bottom), center, angle),
    ];
    draw_quad(d, body[0], body[1], body[2], body[3], Color::new(235, 235, 240, 255));

    let point = [
        rotate_around(Vector2::new(left, body_bottom), center, angle),
        rotate_around(Vector2::new(right, body_bottom), center, angle),
        rotate_around(Vector2::new(center.x, tip), center, angle),
    ];
    d.draw_triangle(point[0], point[1], point[2], Color::new(70, 55, 45, 255));
}

/// Author-requested: icon-only Eraser button — classic two-tone (pink cap /
/// white body) eraser glyph, diagonal cut. Shown only while erasing is
/// active — the default (paint-mode) icon is `draw_pencil_icon`.
fn draw_eraser_icon(d: &mut impl RaylibDraw, r: Rectangle) {
    let w = 16.0;
    let h = 11.0;
    let x = r.x + (r.width - w) / 2.0;
    let y = r.y + (r.height - h) / 2.0;
    let body = Rectangle::new(x, y, w, h);
    d.draw_rectangle_rounded(body, 0.3, 4, Color::new(235, 235, 240, 255));
    d.draw_triangle(
        Vector2::new(x, y + h),
        Vector2::new(x, y),
        Vector2::new(x + w * 0.55, y),
        Color::new(235, 120, 150, 255),
    );
    d.draw_rectangle_rounded_lines(body, 0.3, 4, Color::new(40, 40, 48, 255));
}

/// Author-requested: icon-only Lock button — padlock glyph, shackle swung
/// open when unlocked so the two states read apart even in grayscale.
fn draw_lock_icon(d: &mut impl RaylibDraw, r: Rectangle, locked: bool) {
    let cx = r.x + r.width / 2.0;
    let body_w = 14.0;
    let body_h = 10.0;
    let body_y = r.y + r.height / 2.0 - 1.0;
    let body = Rectangle::new(cx - body_w / 2.0, body_y, body_w, body_h);
    let shackle_cy = body_y - 2.0;
    let icon_color = Color::new(235, 235, 240, 255);
    if locked {
        d.draw_ring(Vector2::new(cx, shackle_cy), 3.5, 5.5, 180.0, 360.0, 16, icon_color);
    } else {
        d.draw_ring(Vector2::new(cx + 3.0, shackle_cy), 3.5, 5.5, 180.0, 340.0, 16, icon_color);
    }
    d.draw_rectangle_rounded(body, 0.25, 4, icon_color);
}

/// F9.6 item 5: minimal controls list, toggled by Escape (see `handle_input`).
fn draw_help_overlay(d: &mut impl RaylibDraw) {
    d.draw_rectangle_rec(Rectangle::new(0.0, 0.0, SCREEN_W, SCREEN_H), Color::new(0, 0, 0, 140));

    let o = overlay_rect();
    d.draw_rectangle_rec(o, Color::new(24, 24, 30, 250));
    d.draw_rectangle_lines_ex(o, 2.0, Color::new(90, 90, 100, 255));
    d.draw_text("Controls", o.x as i32 + 20, o.y as i32 + 14, 18, Color::RAYWHITE);

    let close = overlay_close_rect();
    d.draw_rectangle_rec(close, Color::new(60, 40, 40, 255));
    d.draw_text("X", close.x as i32 + 11, close.y as i32 + 7, 16, Color::RAYWHITE);

    const LINES: &[&str] = &[
        "Left-drag on your island or the margin: paint",
        "X or the Eraser button: toggle paint/erase",
        "Middle-click a painted tile: eyedropper (must be unlocked)",
        "Long-press a foreign tile: merge/take its color",
        "Double-click/-tap a foreign island: like / unlike",
        "Hover (or tap) a foreign island: info",
        "WASD / arrow keys: pan     Q / E: zoom",
        "Right-drag, middle-drag, or Shift+left-drag: pan",
        "Mouse wheel / pinch: zoom",
        "Escape: close this / any open panel",
    ];
    let mut ty = o.y as i32 + 54;
    for line in LINES {
        d.draw_text(line, o.x as i32 + 20, ty, 15, Color::RAYWHITE);
        ty += 26;
    }
}

fn draw_overlay(d: &mut impl RaylibDraw, state: &UiState, info: &HudInfo, mouse: Vector2) {
    // Dim the world behind the modal.
    d.draw_rectangle_rec(Rectangle::new(0.0, 0.0, SCREEN_W, SCREEN_H), Color::new(0, 0, 0, 140));

    let o = overlay_rect();
    d.draw_rectangle_rec(o, Color::new(24, 24, 30, 250));
    d.draw_rectangle_lines_ex(o, 2.0, Color::new(90, 90, 100, 255));
    d.draw_text("Inventory", o.x as i32 + 20, o.y as i32 + 14, 18, Color::RAYWHITE);

    let close = overlay_close_rect();
    d.draw_rectangle_rec(close, Color::new(60, 40, 40, 255));
    d.draw_text("X", close.x as i32 + 11, close.y as i32 + 7, 16, Color::RAYWHITE);

    // F9.6 item 4: sorted by hue (see `sorted_hues`), and a hex-code label
    // pops up above whichever swatch the mouse is currently over.
    let mut hovered_hex: Option<(Rectangle, String)> = None;
    for (i, &hue) in sorted_hues(info).iter().enumerate() {
        let r = swatch_rect(i);
        let color = swatch_color(hue, info);
        d.draw_rectangle_rec(r, color);
        // Compare against the Hue slider's anchor, not the live (possibly
        // nudged) brush hue — otherwise dragging the slider away from 0
        // makes every swatch look unselected even though you're still
        // fine-tuning the same one.
        let selected = hue == state.base_hue;
        d.draw_rectangle_lines_ex(
            r,
            if selected { 3.0 } else { 1.0 },
            if selected { Color::GOLD } else { Color::new(200, 200, 200, 160) },
        );
        if point_in(mouse, r) {
            hovered_hex = Some((r, color_hex(color)));
        }
    }
    if let Some((r, hex)) = hovered_hex {
        let label_w = 8.0 * hex.len() as f32 + 8.0;
        let label = Rectangle::new(r.x, r.y - 20.0, label_w, 18.0);
        d.draw_rectangle_rec(label, Color::new(10, 10, 14, 235));
        d.draw_rectangle_lines_ex(label, 1.0, Color::new(120, 120, 130, 200));
        d.draw_text(&hex, label.x as i32 + 4, label.y as i32 + 2, 13, Color::RAYWHITE);
    }

    let tol = world::constants::HUE_TOLERANCE;
    let offset = hue_offset_signed(effective_hue(state, info), state.base_hue).clamp(-tol, tol);
    draw_hue_slider(d, hue_slider_rect(), offset, tol);
    draw_slider(d, sat_slider_rect(), info.brush.1, info.sat_cap, &format!("Saturation ({})", info.brush.1));
    draw_slider(d, val_slider_rect(), info.brush.2, 100, &format!("Value ({})", info.brush.2));
}

/// Copy/import ID (Cookie-Clicker-style account portability) + reset. Shares
/// the inventory overlay's footprint/backdrop/close button but never draws
/// alongside it (`draw` picks one or the other).
fn draw_account_overlay(d: &mut impl RaylibDraw, state: &UiState, info: &HudInfo) {
    d.draw_rectangle_rec(Rectangle::new(0.0, 0.0, SCREEN_W, SCREEN_H), Color::new(0, 0, 0, 140));

    let o = overlay_rect();
    d.draw_rectangle_rec(o, Color::new(24, 24, 30, 250));
    d.draw_rectangle_lines_ex(o, 2.0, Color::new(90, 90, 100, 255));
    d.draw_text("Account", o.x as i32 + 20, o.y as i32 + 14, 18, Color::RAYWHITE);

    let close = overlay_close_rect();
    d.draw_rectangle_rec(close, Color::new(60, 40, 40, 255));
    d.draw_text("X", close.x as i32 + 11, close.y as i32 + 7, 16, Color::RAYWHITE);

    d.draw_text(&format!("Signed in as {}", info.short_id), o.x as i32 + 20, o.y as i32 + 44, 14, Color::LIGHTGRAY);

    let cb = copy_btn_rect();
    d.draw_rectangle_rec(cb, Color::new(40, 40, 48, 255));
    d.draw_text("Copy full ID", cb.x as i32 + 16, cb.y as i32 + 11, 14, Color::RAYWHITE);
    if state.copy_clicked_at.is_some_and(|t| t.elapsed() < TOAST_DURATION) {
        d.draw_text(
            "copied (or check the popup)",
            cb.x as i32 + cb.width as i32 + 12,
            cb.y as i32 + 11,
            14,
            Color::LIGHTGRAY,
        );
    }
    d.draw_text(
        "Save this before clearing site data or switching browsers/devices.\nKeep it private: anyone who has it can log in as you.",
        o.x as i32 + 20,
        cb.y as i32 + cb.height as i32 + 10,
        12,
        Color::new(220, 170, 90, 255),
    );

    if info.show_token_import {
        let field = import_field_rect();
        d.draw_text("Paste an ID to restore that account:", field.x as i32, field.y as i32 - 18, 14, Color::LIGHTGRAY);
        d.draw_rectangle_rec(field, Color::new(28, 28, 34, 255));
        d.draw_rectangle_lines_ex(field, 1.0, if state.import_focused { Color::GOLD } else { Color::new(90, 90, 96, 255) });
        let shown = if state.import_input.is_empty() && !state.import_focused { "paste here..." } else { &state.import_input };
        d.draw_text(shown, field.x as i32 + 6, field.y as i32 + 7, 14, Color::RAYWHITE);

        let ib = import_btn_rect();
        d.draw_rectangle_rec(ib, Color::new(40, 40, 48, 255));
        d.draw_text("Import", ib.x as i32 + 20, ib.y as i32 + 9, 14, Color::RAYWHITE);
    }

    let rb = reset_btn_rect();
    let armed = state.reset_armed_at.is_some();
    d.draw_rectangle_rec(rb, if armed { Color::new(140, 50, 50, 255) } else { Color::new(60, 40, 40, 255) });
    d.draw_text(
        if armed { "Click again to confirm reset" } else { "Reset account" },
        rb.x as i32 + 10,
        rb.y as i32 + 11,
        14,
        Color::RAYWHITE,
    );
    d.draw_text(
        "Wipes XP and unlocked colors, rolls a new starting hue.\nKeeps your ID, name, and island art.",
        rb.x as i32,
        rb.y as i32 + rb.height as i32 + 10,
        12,
        Color::GRAY,
    );
}

/// Signed ±`tol` slider: a center tick at offset 0 plus a handle that can
/// land either side of it, unlike the one-directional Saturation/Value bars.
fn draw_hue_slider(d: &mut impl RaylibDraw, track: Rectangle, offset: i32, tol: i32) {
    let label = if offset == 0 { "Hue (0)".to_string() } else { format!("Hue ({offset:+})") };
    d.draw_text(&label, track.x as i32, track.y as i32 - 18, 14, Color::LIGHTGRAY);
    d.draw_rectangle_rec(track, Color::new(50, 50, 58, 255));
    let mid_x = track.x + track.width / 2.0;
    d.draw_line_ex(
        Vector2::new(mid_x, track.y),
        Vector2::new(mid_x, track.y + track.height),
        1.0,
        Color::new(200, 200, 200, 140),
    );
    let frac = (offset + tol) as f32 / (2 * tol) as f32;
    let handle_x = track.x + track.width * frac;
    d.draw_circle(handle_x as i32, (track.y + track.height / 2.0) as i32, 9.0, Color::RAYWHITE);
}

fn draw_slider(d: &mut impl RaylibDraw, track: Rectangle, value: u8, max: u8, label: &str) {
    d.draw_text(label, track.x as i32, track.y as i32 - 18, 14, Color::LIGHTGRAY);
    d.draw_rectangle_rec(track, Color::new(50, 50, 58, 255));
    let frac = if max == 0 { 0.0 } else { value as f32 / max as f32 };
    let filled = Rectangle::new(track.x, track.y, track.width * frac, track.height);
    d.draw_rectangle_rec(filled, Color::new(120, 160, 220, 255));
    let handle_x = track.x + track.width * frac;
    d.draw_circle(handle_x as i32, (track.y + track.height / 2.0) as i32, 9.0, Color::RAYWHITE);
}
