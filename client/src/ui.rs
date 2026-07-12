//! Immediate-mode HUD: header, footer, and the inventory/color-picker
//! overlay. Raylib primitives only — no new dependencies. `main.rs` calls
//! `handle_input` (before `begin_drawing`, since text/mouse input needs
//! `&mut RaylibHandle`) then `draw` (during `begin_drawing`). Layout
//! constants here are the shared source of truth for the header/footer band
//! heights so the map viewport can size around them.

use raylib::prelude::*;
use std::collections::{HashMap, VecDeque};
use std::sync::OnceLock;
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

/// F13 follow-up: the paint/erase footer toggle grew a third state — Move,
/// which makes plain left-drag pan the camera instead of painting/merging
/// (no Shift/right-click needed). Cycled by `X` or the footer button, in
/// this order: Paint -> Erase -> Move -> Paint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tool {
    Paint,
    Erase,
    Move,
    Eyedropper,
}

impl Tool {
    fn cycle(self) -> Tool {
        match self {
            Tool::Paint => Tool::Erase,
            Tool::Erase => Tool::Move,
            Tool::Move => Tool::Paint,
            Tool::Eyedropper => Tool::Paint,
        }
    }
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
/// `merge_from`, when set (cursor-merge only — see `show_merge_toast`), adds
/// the two PRE-merge hues so the toast can render "mine + his = new" instead
/// of just the result.
struct Toast {
    text: String,
    hue: Option<u16>,
    merge_from: Option<(u16, u16)>,
    shown_at: Instant,
}

/// A concrete paint color, independent from the inventory entry whose
/// ±7° hue window authorizes it. Recent colors preserve all three channels
/// so selecting one reproduces exactly what was drawn/merged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecentColor {
    pub hue: u16,
    pub sat: u8,
    pub val: u8,
}

pub struct UiState {
    /// Title screen (author-requested): shown from launch until the Draw
    /// button (or Enter) dismisses it. While set, `handle_input` swallows
    /// all HUD input and `draw` renders the title overlay instead of the
    /// HUD; the map itself stays visible behind the semi-transparent
    /// backdrop (the clients hold the camera on the whole-world pose, see
    /// `main.rs`/`bin/web.rs`). Folded into `any_modal_open` so every
    /// map-input gate treats it like a modal.
    pub title_active: bool,
    /// Animation clock for the title screen (logo shimmer, button pulse).
    title_started: Instant,
    pub name_input: String,
    name_loaded: bool,
    name_focused: bool,
    overlay_open: bool,
    recent_open: bool,
    pub recent_colors: VecDeque<RecentColor>,
    dragging: Drag,
    toast: Option<Toast>,
    /// A confirmed local Hexa reward opens this acknowledgement modal.
    hexa_success_open: bool,
    /// HEXA stays visible in the world briefly before its acknowledgement
    /// modal covers it. Pending success is deliberately not modal yet.
    hexa_success_pending_at: Option<Instant>,
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
    account_open: bool,
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
    /// F9.6 item 1 (extended by the F13 follow-up's Move state): paint/
    /// erase/move tool — footer button or the `X` key (not `E`, which item 6
    /// claims for keyboard zoom-in) cycles Paint -> Erase -> Move -> Paint.
    pub tool: Tool,
    /// True when Eyedropper temporarily enabled Lock for a player who was
    /// previously unlocked. Leaving the tool restores that prior state;
    /// players who entered already locked remain locked.
    eyedropper_restore_unlock: bool,
    /// F9.6 item 5: minimal keybindings/help overlay, opened by Escape when
    /// nothing else is open (closed by Escape again, matching item 4's rule
    /// for every other overlay). Mutually exclusive with the other three.
    help_open: bool,
    /// Author-requested: saturation/lightness is tied to each unlocked hue
    /// individually, not shared across the whole Inventory page — dragging
    /// the sliders while swatch A is selected must not visually shift every
    /// other swatch in the grid. Populated lazily as the player tunes a
    /// color; an absent entry means "still at the canonical default" (see
    /// `default_sat`/`DEFAULT_VAL`).
    swatch_hsl: HashMap<u16, (u8, u8)>,
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
    /// Author-requested: whether the island's border is currently hidden
    /// (`disable_island_border`) — drives the popup's toggle button. Only
    /// meaningful when `is_own`.
    pub border_hidden: bool,
    /// Author-requested: the border's current color — whatever `border_color`
    /// resolves to (the custom pin, or the seed-hue default when unset),
    /// regardless of `border_hidden`. Precomputed by the caller (same
    /// fallback the map-render code uses) so this module stays free of the
    /// packed-HSV/seed-hue lookup. Drives the "Set border to current color"
    /// button's before/after preview swatch. Only meaningful when `is_own`.
    pub border_color: Color,
}

impl UiState {
    pub fn new() -> Self {
        Self {
            title_active: true,
            title_started: Instant::now(),
            name_input: String::new(),
            name_loaded: false,
            name_focused: false,
            overlay_open: false,
            recent_open: false,
            recent_colors: VecDeque::new(),
            dragging: Drag::None,
            toast: None,
            hexa_success_open: false,
            hexa_success_pending_at: None,
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
            tool: Tool::Paint,
            eyedropper_restore_unlock: false,
            help_open: false,
            swatch_hsl: HashMap::new(),
        }
    }

    /// F9.6 item 6: whether a text field currently owns keyboard input —
    /// `main.rs`/`bin/web.rs` check this before letting WASD/arrows/Q/E pan
    /// or zoom the camera, so typing a name doesn't also drive it.
    pub fn text_field_focused(&self) -> bool {
        self.name_focused || self.import_focused || self.link_edit_focused
    }

    /// Arms the eyedropper and reports whether the caller must enable Lock.
    pub fn arm_eyedropper(&mut self, currently_locked: bool) -> bool {
        self.tool = Tool::Eyedropper;
        self.eyedropper_restore_unlock = !currently_locked;
        !currently_locked
    }

    /// Returns to Paint and reports whether a temporary eyedropper Lock
    /// should be removed.
    pub fn finish_eyedropper(&mut self) -> bool {
        self.tool = Tool::Paint;
        std::mem::take(&mut self.eyedropper_restore_unlock)
    }

    /// Author-requested: called by the caller right after firing
    /// `like_island`/`unlike_island` from a map double-click, so the click
    /// gets a little visual acknowledgement even though the popup never
    /// opens for that gesture. `pos` is the click's screen position.
    pub fn spawn_like_anim(&mut self, pos: Vector2, liked: bool) {
        self.like_anims.push(LikeAnim { pos, liked, started_at: Instant::now() });
    }

    /// True while any of the four "real" modals — Colors, Account, Help, or
    /// the own-island popup — is open, i.e. the map/world should be fully
    /// gated. Deliberately excludes the foreign-island hover tooltip (no
    /// interactive chrome of its own, and blocking input while it's up
    /// would break the very double-click/long-press gestures it's showing
    /// info for — see `open_island_info`). Single source of truth for
    /// `main.rs`/`bin/web.rs`, which used to each hand-roll this same
    /// four-flag check and could drift out of sync — the web build was
    /// missing two of the four, letting a hovered island silently close
    /// My Isle or the Escape/help overlay. The title screen counts too —
    /// it covers the whole screen, so the map must be fully gated (no
    /// painting, panning, gift claims, hover popups, or `set_pos`
    /// heartbeat) until Draw dismisses it.
    pub fn any_modal_open(&self) -> bool {
        self.title_active
            || self.overlay_open
            || self.recent_open
            || self.account_open
            || self.help_open
            || self.hexa_success_open
            || self.island_popup.as_ref().is_some_and(|p| p.is_own)
    }

    /// Closes all four modals — the group is mutually exclusive by
    /// convention, enforced here in the one place that needs to open a
    /// different one, instead of every call site repeating the same four
    /// assignments.
    fn close_all_modals(&mut self) {
        self.overlay_open = false;
        self.recent_open = false;
        self.account_open = false;
        self.help_open = false;
        self.hexa_success_open = false;
        self.island_popup = None;
        self.dragging = Drag::None;
    }

    /// Opens the F8 island-info popup, closing the other three (mutually
    /// exclusive) modals if any was open.
    pub fn open_island_info(&mut self, info: IslandInfo) {
        self.close_all_modals();
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
    pub fn refresh_island_popup(&mut self, likes: u32, already_liked: bool, border_hidden: bool, border_color: Color) {
        if let Some(popup) = &mut self.island_popup {
            popup.likes = likes;
            popup.already_liked = already_liked;
            popup.border_hidden = border_hidden;
            popup.border_color = border_color;
        }
    }

    /// Called by `main.rs` when it sees a fresh `inventory` row belonging to
    /// the caller (cursor- or tile-merge). `partner_label` is the other
    /// player's name if set, else their short identity hex. `merge_from`
    /// is `Some((my_hue, partner_hue))` — the two
    /// PRE-merge hues, looked up by the caller against the matching
    /// `MergeEvent` row — for a real two-player cursor-merge; `None` for a
    /// tile-merge/eyedrop, which has no second live hue to show.
    pub fn show_merge_toast(&mut self, color: RecentColor, partner_label: &str, merge_from: Option<(u16, u16)>) {
        self.toast = Some(Toast {
            text: format!("new color, obtained with {partner_label}"),
            hue: Some(color.hue),
            merge_from,
            shown_at: Instant::now(),
        });
        self.note_used_color(color);
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
            merge_from: None,
            shown_at: Instant::now(),
        });
    }

    /// Shown once when a player crosses the level-3 HEXA unlock threshold.
    pub fn show_hexa_unlocked_toast(&mut self) {
        self.toast = Some(Toast {
            text: "HEXA UNLOCKED! Meet your friends at the centre of the world!".to_string(),
            hue: None,
            merge_from: None,
            shown_at: Instant::now(),
        });
    }

    /// F9.6 item 2: plain-text toast (no flash swatch) — used for the
    /// middle-click eyedropper's "not unlocked" feedback.
    pub fn show_info_toast(&mut self, text: String) {
        self.toast = Some(Toast { text, hue: None, merge_from: None, shown_at: Instant::now() });
    }

    /// F11: called by the caller when it sees a fresh `inventory` row with
    /// `from_gift: true` — a flying-gift hue win. Same flash-swatch
    /// treatment as `show_merge_toast`, distinct wording since there's no
    /// merge partner to name.
    pub fn show_gift_toast(&mut self, hue: u16) {
        self.toast = Some(Toast { text: "gift claimed — new color!".to_string(), hue: Some(hue), merge_from: None, shown_at: Instant::now() });
        self.note_used_color(RecentColor { hue, sat: default_sat(), val: DEFAULT_VAL });
    }

    /// F13: called by the caller when it sees a fresh `inventory` row that
    /// time-joins a `hexa_event` row — a Hexa-event pooled hue, distinct
    /// from both a merge (which always names a partner) and a gift/reset
    /// (neither of which has an event to join).
    pub fn show_hexa_toast(&mut self, hue: u16) {
        self.toast = Some(Toast { text: "Hexa event! colors pooled with 5 others".to_string(), hue: Some(hue), merge_from: None, shown_at: Instant::now() });
        self.note_used_color(RecentColor { hue, sat: default_sat(), val: DEFAULT_VAL });
    }

    /// Prominent acknowledgement for completing the six-player formation.
    /// Kept separate from `Toast`: several pooled inventory rows can arrive
    /// together, but they should all refer to one shared modal.
    pub fn show_hexa_success_popup(&mut self) {
        self.hexa_success_pending_at = Some(Instant::now());
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

    /// Most-recently-used first, exact-HSL deduped, capped at the modal's
    /// 9×11 capacity. The footer simply previews the first three entries.
    pub fn note_used_color(&mut self, color: RecentColor) {
        self.recent_colors.retain(|&c| c != color);
        self.recent_colors.push_front(color);
        self.recent_colors.truncate(99);
    }

    /// F9.5 item 4 (author-caught, `known_bugs.md`: "the selected color is
    /// not in the recent color used on launch"): seed the footer's last-3
    /// ring with the caller's own starting/current hue the first time it's
    /// known, so a brand-new connection shows something there instead of
    /// staying empty until the player's first merge or swatch click. A no-op
    /// once the recent history has any entry, so the caller can call this every
    /// frame after `me` becomes known rather than tracking its own
    /// seed-once flag.
    pub fn seed_recent_once(&mut self, color: RecentColor) {
        if self.recent_colors.is_empty() {
            self.recent_colors.push_front(color);
        }
    }

    /// F9.5 item 4 (author-caught: "reset account doesn't reset the last 3
    /// selected colors"): `reset_account` wipes the caller's entire
    /// inventory down to one fresh hue, but prepending alone would just
    /// prepend that hue onto the EXISTING ring, leaving up to two
    /// now-meaningless pre-reset colors still showing. Clears first.
    pub fn note_reset_hue(&mut self, hue: u16) {
        self.recent_colors.clear();
        self.recent_colors.push_front(RecentColor { hue, sat: default_sat(), val: DEFAULT_VAL });
    }

    /// A hue's own remembered sat/val, or the canonical default if the
    /// player has never tuned that color.
    fn tile_hsl(&self, hue: u16) -> (u8, u8) {
        self.swatch_hsl.get(&hue).copied().unwrap_or((default_sat(), DEFAULT_VAL))
    }

    /// Called whenever a Saturation/Lightness drag lands while `hue` is the
    /// selected swatch, so that tuning survives switching to another color
    /// and back.
    fn set_tile_hsl(&mut self, hue: u16, sat: u8, val: u8) {
        self.swatch_hsl.insert(hue, (sat, val));
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
    /// Camera world-space target, shown as X/Y in the header.
    pub camera_target: Vector2,
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
    /// The caller's own island's itch.io rate id, if set — feeds the footer
    /// quick-access link field (`footer_link_edit_rect`), which syncs its
    /// display buffer from this every frame it isn't focused.
    pub link_id: Option<u32>,
}

#[derive(Default)]
pub struct Actions {
    pub set_brush: Option<(u16, u8, u8)>,
    pub set_name: Option<String>,
    pub set_lock: Option<bool>,
    pub center_camera: bool,
    /// World Centre footer button: zoom out to frame the whole world instead
    /// of the caller's own island.
    pub center_world: bool,
    /// Header's export control: the caller temporarily frames the player's
    /// island, renders the clean share card, then captures that frame.
    pub export_screenshot: bool,
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
    /// The "Your link" row in the caller's own island popup was clicked.
    /// Opens the URL only — `click_link` is deliberately NOT fired here, the
    /// server rejects self-clicks (see `click_link`'s reducer doc comment).
    pub open_own_link: Option<u32>,
    /// Author-requested: pin the caller's own island's border to their
    /// current brush color (and un-hide it if it was disabled).
    pub set_island_border: bool,
    /// Author-requested: hide the caller's own island's border entirely.
    /// Fired by the popup's Border: Shown/Hidden toggle when currently shown.
    pub disable_island_border: bool,
    /// Author-requested: re-show a previously hidden border WITHOUT touching
    /// `border_color` — fired by the same toggle button when currently
    /// hidden. Kept distinct from `set_island_border`, which also repins the
    /// color to the current brush.
    pub show_island_border: bool,
}

fn footer_bg() -> Rectangle {
    Rectangle::new(0.0, SCREEN_H - FOOTER_H, SCREEN_W, FOOTER_H)
}

/// A compact, icon-only share/export control. It is deliberately in the
/// header (rather than the already busy footer) so taking a screenshot is a
/// global action that remains easy to find on touch devices.
fn export_btn_rect() -> Rectangle {
    Rectangle::new(SCREEN_W - 34.0, 3.0, 28.0, 22.0)
}

/// Padding from the screen's left/right edges for the two buttons now
/// pinned to the footer's outer corners (`inventory_btn_rect`/"Colors" on
/// the left, `center_btn_rect`/"Centre" on the right).
const FOOTER_EDGE_PAD: f32 = 8.0;

/// Author-requested: "Colors" now anchors the very bottom-left corner of
/// the footer (previously it sat mid-cluster, right of `center_btn_rect`).
fn inventory_btn_rect() -> Rectangle {
    Rectangle::new(FOOTER_EDGE_PAD, SCREEN_H - FOOTER_H + 7.0, 88.0, 30.0)
}

/// Author-requested: a second recenter button, right of Isle Centre, that
/// zooms out to frame the whole world (`world_fit`) instead of the caller's
/// own island. Anchors the very bottom-right corner of the footer.
fn world_centre_btn_rect() -> Rectangle {
    Rectangle::new(SCREEN_W - FOOTER_EDGE_PAD - 30.0, SCREEN_H - FOOTER_H + 7.0, 30.0, 30.0)
}

/// Author-requested: icon-only (a "recenter"/geolocation glyph, see
/// `draw_locate_icon`), renamed "Isle Centre" now that `world_centre_btn_rect`
/// covers the whole-world case — sits directly left of it.
fn center_btn_rect() -> Rectangle {
    let wb = world_centre_btn_rect();
    Rectangle::new(wb.x - 4.0 - 30.0, SCREEN_H - FOOTER_H + 7.0, 30.0, 30.0)
}

/// Author-requested: the name field is centered on screen, with the last-3
/// swatches/eraser/lock built outward to its left (see below). Account/My
/// Isle no longer flank it — they now sit next to `center_btn_rect` instead.
fn name_field_rect() -> Rectangle {
    // Widened from 150: shrinking `footer_link_edit_rect` (a 7-digit id
    // needs far less room than a name) freed up space to its right.
    let w = 170.0;
    Rectangle::new(342.0, SCREEN_H - FOOTER_H + 7.0, w, 30.0)
}

/// Author-requested: icon-only (see `draw_footer`), sitting directly left of
/// the name field.
fn lock_btn_rect() -> Rectangle {
    let bb = brush_btn_rect();
    Rectangle::new(bb.x - 4.0 - 30.0, SCREEN_H - FOOTER_H + 7.0, 30.0, 30.0)
}

/// Author-requested: icon-only (see `draw_footer`), left of the lock toggle.
fn eraser_btn_rect() -> Rectangle {
    let lb = lock_btn_rect();
    Rectangle::new(lb.x - 4.0 - 30.0, SCREEN_H - FOOTER_H + 7.0, 30.0, 30.0)
}

/// Eyedropper tool button: icon-only, mirroring the Eraser/Lock
/// cluster's placement but on the OTHER side of the name field (the only
/// free strip left in the footer at this screen width — the left side is
/// already packed edge-to-edge with Colors/last3/Eraser/Lock/name field, and
/// Account/My Isle/Center already claim the far right). Clicking it arms a
/// one-shot click/tap selection; middle-click remains the desktop shortcut.
fn brush_btn_rect() -> Rectangle {
    let nf = name_field_rect();
    Rectangle::new(nf.x - 8.0 - 30.0, SCREEN_H - FOOTER_H + 7.0, 30.0, 30.0)
}

fn recent_btn_rect() -> Rectangle {
    let eb = eraser_btn_rect();
    Rectangle::new(eb.x - 4.0 - 30.0, SCREEN_H - FOOTER_H + 7.0, 30.0, 30.0)
}

fn last3_rect(i: usize) -> Rectangle {
    let rb = recent_btn_rect();
    let group_x0 = rb.x - 4.0 - (2.0 * 34.0 + 30.0);
    Rectangle::new(group_x0 + i as f32 * 34.0, SCREEN_H - FOOTER_H + 7.0, 30.0, 30.0)
}

const RECENT_SWATCH: f32 = 42.0;
const RECENT_GAP: f32 = 6.0;
const RECENT_COLS: usize = 11;

fn recent_swatch_rect(i: usize) -> Rectangle {
    let o = overlay_rect();
    let col = (i % RECENT_COLS) as f32;
    let row = (i / RECENT_COLS) as f32;
    Rectangle::new(
        o.x + 39.0 + col * (RECENT_SWATCH + RECENT_GAP),
        o.y + 58.0 + row * (RECENT_SWATCH + RECENT_GAP),
        RECENT_SWATCH,
        RECENT_SWATCH,
    )
}

/// Author-requested: a footer button to open the caller's own island-info
/// popup, replacing the old "click your own island" gesture (which just
/// painted the cell it was released on, so the popup never actually showed).
/// Now sits directly left of `center_btn_rect` rather than next to the name
/// field.
fn my_island_btn_rect() -> Rectangle {
    let cb = center_btn_rect();
    Rectangle::new(cb.x - 8.0 - 56.0, SCREEN_H - FOOTER_H + 7.0, 56.0, 30.0)
}

/// Author-requested: moved out of the footer and into the header, sitting
/// directly left of the Export button (matching its height/y so the two
/// read as one row of header controls).
fn account_btn_rect() -> Rectangle {
    let export = export_btn_rect();
    Rectangle::new(export.x - 8.0 - 60.0, 3.0, 60.0, 22.0)
}

fn overlay_rect() -> Rectangle {
    Rectangle::new(60.0, 60.0, 600.0, 560.0)
}

fn overlay_close_rect() -> Rectangle {
    let o = overlay_rect();
    Rectangle::new(o.x + o.width - 38.0, o.y + 8.0, 30.0, 30.0)
}

/// The acknowledgement control in the HEXA completion modal. Kept as a
/// shared layout helper so its hitbox and drawn button cannot drift apart.
fn hexa_success_ok_rect() -> Rectangle {
    Rectangle::new(SCREEN_W / 2.0 - 78.0, 540.0, 156.0, 42.0)
}

/// Whether this frame's click should dismiss whichever modal occupies the
/// shared panel footprint (`overlay_rect()`) — either its close button, or
/// anywhere outside the panel (F9.6 item 4/5's rule, applied uniformly to
/// all four modals through this one helper instead of four separate copies).
fn modal_dismiss_clicked(mouse: Vector2, clicked: bool) -> bool {
    clicked && (point_in(mouse, overlay_close_rect()) || !point_in(mouse, overlay_rect()))
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

/// F9: own-island popup only — numeric input for the itch.io rate id. Fills
/// the row's full width now that there's no separate Set button (submits
/// automatically as you type, see `handle_input`).
fn link_edit_rect() -> Rectangle {
    let o = overlay_rect();
    Rectangle::new(o.x + 20.0, o.y + 180.0, 310.0, 32.0)
}

/// Author-requested: footer quick-access shortcut for the itch.io rate id —
/// fills the gap left by moving the Account button into the header, so a
/// player can set/see their island's jam link without opening the full My
/// Isle popup. Shares `link_edit_input`/`link_edit_focused` with
/// `link_edit_rect` above (same buffer, same typing handler in
/// `handle_input`) rather than tracking a second one.
fn footer_link_edit_rect() -> Rectangle {
    // 60 wide: a submission id is always exactly 7 digits (see
    // `handle_link_edit_typing`'s cap), so it never needs the room a free-
    // text field would.
    let mb = my_island_btn_rect();
    Rectangle::new(mb.x - 8.0 - 60.0, SCREEN_H - FOOTER_H + 7.0, 60.0, 30.0)
}

/// Own-island popup only — the "Your link" row, clickable once a link is
/// set (see `handle_input`'s own-popup block).
fn link_row_rect() -> Rectangle {
    let o = overlay_rect();
    Rectangle::new(o.x + 20.0, o.y + 130.0, 320.0, 20.0)
}

/// Author-requested: itch.io jam rate ids are always 7 digits — `Some` only
/// at exactly that length, `None` if shorter or longer. This is the "Your
/// link" row's clickable/blue condition; red covers the rest, grey is the
/// separate empty-field case (see `draw_island_popup`). Checked against the
/// live edit field rather than the server-confirmed id, so it turns blue
/// the instant the 7th digit is typed instead of waiting on a round-trip.
fn typed_link_id(state: &UiState) -> Option<u32> {
    let typed = state.link_edit_input.trim();
    if typed.len() != 7 {
        return None;
    }
    typed.parse().ok()
}

/// F9: digits-only typing for `link_edit_input`, submitting every keystroke
/// that parses (no separate Set button). Shared by both places that can
/// focus the field — the My Isle popup's own row and the footer's
/// quick-access shortcut (`footer_link_edit_rect`) — so the two can never
/// drift out of sync with each other. Capped at 7 digits: a raylib gamejam
/// submission rate id is always exactly 7 (see `typed_link_id`), so a
/// longer typed string could never be valid anyway.
// Takes the buffer by its own `&mut String` (not `&mut UiState`) so callers
// can invoke this while another field of `state` (e.g. `island_popup`) is
// still borrowed — the popup call site needs exactly that.
fn handle_link_edit_typing(rl: &mut RaylibHandle, input: &mut String, actions: &mut Actions) {
    let mut changed = false;
    while let Some(c) = rl.get_char_pressed() {
        if c.is_ascii_digit() && input.len() < 7 {
            input.push(c);
            changed = true;
        }
    }
    if rl.is_key_pressed(KeyboardKey::KEY_BACKSPACE) {
        input.pop();
        changed = true;
    }
    if changed {
        if let Ok(id) = input.trim().parse::<u32>() {
            actions.set_island_link = Some(id);
        }
    }
}

/// Author-requested: own-island popup only — pin the border to the caller's
/// current brush color. Draws a before (border) `->` after (cursor) preview,
/// see `draw_island_popup`.
fn border_set_btn_rect() -> Rectangle {
    let o = overlay_rect();
    Rectangle::new(o.x + 20.0, o.y + 230.0, 250.0, 36.0)
}

/// Author-requested: own-island popup only — toggles the border between
/// shown and hidden, independent of `border_set_btn_rect`'s color pin. Label
/// reads "Border: Shown"/"Border: Hidden" so the button's own text carries
/// the state, replacing the old separate status line + "Disable border"
/// button.
fn border_toggle_btn_rect() -> Rectangle {
    let o = overlay_rect();
    Rectangle::new(o.x + 320.0, o.y + 230.0, 220.0, 36.0)
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

/// The saturation every player starts with (`world::sat_cap(0)`) — the
/// canonical reference sat for a tile's border and the "Colors" button
/// rainbow, so both compare against the jam's baseline theme rather than
/// whatever the sliders currently sit at. A plain fn (not a const) since
/// `sat_cap` isn't `const fn`.
fn default_sat() -> u8 {
    world::sat_cap(0)
}

/// Canonical reference lightness paired with `default_sat` — matches the
/// merge/level-up toast's flash swatch convention elsewhere in this file.
const DEFAULT_VAL: u8 = 90;

/// A tile's canonical color: the hue at the fixed reference sat/val, never
/// affected by slider drags. Drawn as the tile's border so the player always
/// has something fixed to compare the (possibly tuned) fill against.
fn canonical_color(hue: u16) -> Color {
    world::hsv_color(hue, default_sat(), DEFAULT_VAL)
}

/// Every swatch (footer last-3, inventory grid) is rendered at ITS OWN
/// sat/val — tied per color, not shared across the whole Inventory page —
/// except the one currently selected, which tracks the live (possibly
/// mid-drag) brush so dragging the sliders previews that swatch in real
/// time. See `UiState::tile_hsl`.
fn swatch_color(hue: u16, state: &UiState, info: &HudInfo) -> Color {
    if hue == state.base_hue {
        // The selected tile's fill tracks the LIVE brush, hue included — so
        // nudging the Hue slider (not just Saturation/Lightness) is visible
        // on the tile itself, not just the slider's own gradient.
        world::hsv_color(effective_hue(state, info), info.brush.1, info.brush.2)
    } else {
        let (sat, val) = state.tile_hsl(hue);
        world::hsv_color(hue, sat, val)
    }
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
    if state.hexa_success_pending_at.is_some_and(|at| at.elapsed() >= Duration::from_secs(2)) {
        state.hexa_success_pending_at = None;
        state.close_all_modals();
        state.hexa_success_open = true;
    }

    // Warm the "Colors" button's per-letter glyph-width cache — a no-op
    // after the first call. Must happen here, before `begin_drawing`, since
    // `measure_text` needs a live `RaylibHandle` (see `colors_letter_offsets`).
    colors_letter_offsets(rl);
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
    // Title screen: the Draw button (or Enter) is the only interactive
    // element — swallow everything else. Map-world input is gated separately
    // via `any_modal_open`, and this frame's `over_map_area` was computed
    // while the flag was still set, so the dismissing click can't paint.
    if state.title_active {
        title_label_width(rl); // warm the cache while we still have `rl`
        let mouse = rl.get_mouse_position();
        if (rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT) && point_in(mouse, title_btn_rect()))
            || rl.is_key_pressed(KeyboardKey::KEY_ENTER)
        {
            state.title_active = false;
        }
        return Actions::default();
    }
    // HEXA completion is deliberately acknowledged through its own OK
    // button. It blocks every other input, including Escape and outside
    // clicks, so it cannot be dismissed accidentally while the player is
    // celebrating the rare six-player success.
    if state.hexa_success_open {
        if rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT)
            && point_in(rl.get_mouse_position(), hexa_success_ok_rect())
        {
            state.hexa_success_open = false;
        }
        return Actions::default();
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
    if state.pending_select.is_none() {
        state.set_tile_hsl(state.base_hue, info.brush.1, info.brush.2);
    }
    let mut actions = Actions::default();
    let mouse = rl.get_mouse_position();
    let clicked = rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT);
    let held = rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT);
    let released = rl.is_mouse_button_released(MouseButton::MOUSE_BUTTON_LEFT);

    // F9.6 item 5: Escape closes whatever single modal is open (item 4's
    // rule, applied uniformly to all four — safe to close all of them at
    // once since the group is mutually exclusive by construction); with
    // nothing open, it toggles the help overlay instead. Checked first and
    // returns immediately so a frame that opens/closes a modal doesn't also
    // fall through to the click handling below.
    if rl.is_key_pressed(KeyboardKey::KEY_ESCAPE) {
        if state.any_modal_open() {
            state.close_all_modals();
        } else {
            state.help_open = true;
        }
        return actions;
    }
    // F9.6 item 1 (F13 follow-up: Paint -> Erase -> Move -> Paint) — `X`
    // (not `E`, which item 6 gives to keyboard zoom-in), swallowed while a
    // text field owns keyboard input so typing a name containing "x"
    // doesn't cycle it.
    if rl.is_key_pressed(KeyboardKey::KEY_X) && !state.text_field_focused() {
        if state.tool == Tool::Eyedropper {
            if state.finish_eyedropper() {
                actions.set_lock = Some(false);
            }
        } else {
            state.tool = state.tool.cycle();
        }
    }
    if clicked && point_in(mouse, eraser_btn_rect()) {
        if state.tool == Tool::Eyedropper {
            if state.finish_eyedropper() {
                actions.set_lock = Some(false);
            }
        } else {
            state.tool = state.tool.cycle();
        }
    }
    if clicked && point_in(mouse, brush_btn_rect()) {
        if state.tool == Tool::Eyedropper {
            if state.finish_eyedropper() {
                actions.set_lock = Some(false);
            }
        } else if state.arm_eyedropper(info.locked) {
            actions.set_lock = Some(true);
        }
    }

    if clicked && point_in(mouse, center_btn_rect()) {
        actions.center_camera = true;
    }
    if clicked && point_in(mouse, world_centre_btn_rect()) {
        actions.center_world = true;
    }
    if clicked && point_in(mouse, export_btn_rect()) {
        actions.export_screenshot = true;
        // The header button is a complete gesture of its own. In particular,
        // do not let this same click blur/commit the name field below.
        return actions;
    }
    // Collected first, applied after: `note_used_hue` below needs `&mut
    // state.last3`, which can't happen while this loop still holds `.iter()`
    // borrowed from it.
    let mut last3_clicked = None;
    for (i, &color) in state.recent_colors.iter().take(3).enumerate() {
        if clicked && point_in(mouse, last3_rect(i)) {
            last3_clicked = Some(color);
        }
    }
    // Re-clicking the ALREADY-selected color is a no-op: forcing the brush
    // back to that hue's exact value would silently wipe out any live Hue
    // slider nudge (e.g. dialing in a merge) the player currently has going.
    if let Some(color) = last3_clicked {
        if (color.hue, color.sat.min(info.sat_cap), color.val) != info.brush {
            actions.set_brush = Some((color.hue, color.sat.min(info.sat_cap), color.val));
            state.base_hue = info.hues.iter().copied().min_by_key(|&h| world::hue_dist(h, color.hue)).unwrap_or(color.hue);
            state.pending_select = Some(color.hue);
            state.note_used_color(color);
        }
        // Picking a color implies you want to paint with it, not erase or move.
        if state.tool == Tool::Eyedropper && state.finish_eyedropper() {
            actions.set_lock = Some(false);
        } else {
            state.tool = Tool::Paint;
        }
    }
    if clicked && point_in(mouse, inventory_btn_rect()) {
        let opening = !state.overlay_open;
        state.close_all_modals();
        state.overlay_open = opening;
        // Must return here: the footer button sits below `overlay_rect()`,
        // so without this the "click outside the panel closes it" check
        // further down would see this same click, land outside the panel,
        // and immediately close the overlay it just opened.
        return actions;
    }
    if clicked && point_in(mouse, recent_btn_rect()) {
        let opening = !state.recent_open;
        state.close_all_modals();
        state.recent_open = opening;
        return actions;
    }
    if clicked && point_in(mouse, lock_btn_rect()) && state.tool != Tool::Eyedropper {
        actions.set_lock = Some(!info.locked);
    }
    if clicked && point_in(mouse, account_btn_rect()) {
        let opening = !state.account_open;
        state.close_all_modals();
        state.account_open = opening;
        // Must return here too (mirrors the Colors button above): without
        // it, this same click — outside `overlay_rect()` — falls through to
        // the Account block below and immediately closes the overlay via
        // its own outside-click-close check.
        return actions;
    }
    if clicked && point_in(mouse, my_island_btn_rect()) {
        actions.open_own_island = true;
        state.close_all_modals();
    }

    // Footer quick-access rate-id field (see `footer_link_edit_rect`): only
    // reachable here, since My Isle open takes the early return above and
    // hands typing to its own copy of this same field/handler.
    let footer_link_field = footer_link_edit_rect();
    if clicked {
        state.link_edit_focused = point_in(mouse, footer_link_field);
    }
    if state.link_edit_focused {
        handle_link_edit_typing(rl, &mut state.link_edit_input, &mut actions);
    } else {
        // Keep the buffer synced to server truth while nothing is editing
        // it, so the field never shows a stale id (e.g. right after
        // reconnecting as a different account).
        let synced = info.link_id.map_or(String::new(), |id| id.to_string());
        if state.link_edit_input != synced {
            state.link_edit_input = synced;
        }
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
        if modal_dismiss_clicked(mouse, clicked) {
            state.help_open = false;
        }
        return actions;
    }

    // Account overlay is modal too, and mutually exclusive with the
    // inventory overlay (only one can be open, enforced by the toggles
    // above) — handled and returned here before the inventory-overlay gate.
    if state.account_open {
        if modal_dismiss_clicked(mouse, clicked) {
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
            if modal_dismiss_clicked(mouse, clicked) {
                state.island_popup = None;
                return actions;
            }
            // Author-requested: once the typed id is a well-formed 7-digit
            // itch.io rate id (blue, not red/grey — see `typed_link_id`),
            // the "Your link" row itself opens it. Same click gesture as a
            // foreign island's link row, but `click_link` is deliberately
            // not fired (the server rejects self-clicks; see that reducer's
            // doc comment).
            if let Some(id) = typed_link_id(state) {
                if clicked && point_in(mouse, link_row_rect()) {
                    actions.open_own_link = Some(id);
                }
            }
            // F9: own-island link editing — digits only (it's a numeric
            // itch.io submission id), same Ctrl+V-friendly typing as the
            // Account overlay's token import field. Author-requested: no
            // more Set button — every keystroke that changes the buffer
            // submits immediately if it parses, so the id just stays live.
            let field = link_edit_rect();
            if clicked {
                state.link_edit_focused = point_in(mouse, field);
            }
            if state.link_edit_focused {
                handle_link_edit_typing(rl, &mut state.link_edit_input, &mut actions);
            }
            if clicked && point_in(mouse, border_set_btn_rect()) {
                actions.set_island_border = true;
            }
            if clicked && point_in(mouse, border_toggle_btn_rect()) {
                if popup.border_hidden {
                    actions.show_island_border = true;
                } else {
                    actions.disable_island_border = true;
                }
            }
        }
        return actions;
    }

    if state.recent_open {
        if modal_dismiss_clicked(mouse, clicked) {
            state.recent_open = false;
            return actions;
        }
        let selected = state.recent_colors.iter().enumerate().find_map(|(i, &color)| {
            (clicked && point_in(mouse, recent_swatch_rect(i))).then_some(color)
        });
        if let Some(color) = selected {
            let sat = color.sat.min(info.sat_cap);
            actions.set_brush = Some((color.hue, sat, color.val));
            state.base_hue = info.hues.iter().copied().min_by_key(|&h| world::hue_dist(h, color.hue)).unwrap_or(color.hue);
            state.pending_select = Some(color.hue);
            state.note_used_color(color);
            state.recent_open = false;
            if state.tool == Tool::Eyedropper && state.finish_eyedropper() {
                actions.set_lock = Some(false);
            } else {
                state.tool = Tool::Paint;
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
    // `state.overlay_open` before touching camera/paint input). Closes on
    // its own X button or anywhere outside the panel (F9.6 item 4), via the
    // same `modal_dismiss_clicked` helper the other three modals use.
    if modal_dismiss_clicked(mouse, clicked) {
        state.overlay_open = false;
        state.dragging = Drag::None;
        return actions;
    }
    // F9.6 item 4: swatches sorted by hue — `draw_overlay` iterates the same
    // sorted order so swatch indices (and thus `swatch_rect(i)`) line up
    // between the two.
    for (i, &hue) in sorted_hues(info).iter().enumerate() {
        // Re-clicking the ALREADY-selected tile is a no-op — see the last-3
        // click handler above for why (would wipe out a live Hue nudge).
        if clicked && point_in(mouse, swatch_rect(i)) {
            if hue != state.base_hue {
                // Restore THIS color's own remembered sat/val (F-request: tied
                // per color) rather than carrying over whatever the sliders were
                // last left at from a different swatch.
                let (sat, val) = state.tile_hsl(hue);
                actions.set_brush = Some((hue, sat.min(info.sat_cap), val));
                state.base_hue = hue;
                state.pending_select = Some(hue);
                state.note_used_color(RecentColor { hue, sat, val });
            }
            // Picking a color implies you want to paint with it, not erase or move.
            if state.tool == Tool::Eyedropper && state.finish_eyedropper() {
                actions.set_lock = Some(false);
            } else {
                state.tool = Tool::Paint;
            }
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
                let v = slider_value(sat_slider_rect(), mouse.x, 100.0).min(info.sat_cap);
                if v != info.brush.1 {
                    actions.set_brush = Some((info.brush.0, v, info.brush.2));
                    state.set_tile_hsl(state.base_hue, v, info.brush.2);
                }
            }
            Drag::Val => {
                let v = slider_value(val_slider_rect(), mouse.x, 100.0);
                if v != info.brush.2 {
                    actions.set_brush = Some((info.brush.0, info.brush.1, v));
                    state.set_tile_hsl(state.base_hue, info.brush.1, v);
                }
            }
            Drag::None => {}
        }
    }

    actions
}

pub fn draw(d: &mut impl RaylibDraw, state: &UiState, info: &HudInfo, mouse: Vector2) {
    if state.title_active {
        draw_title_screen(d, state, mouse);
        return;
    }
    draw_header(d, info);
    draw_footer(d, state, info);
    if state.overlay_open {
        draw_overlay(d, state, info, mouse);
    } else if state.recent_open {
        draw_recent_overlay(d, state, mouse);
    } else if state.account_open {
        draw_account_overlay(d, state, info);
    } else if let Some(popup) = &state.island_popup {
        if popup.is_own {
            draw_island_popup(d, state, popup, info);
        } else {
            draw_island_tooltip(d, popup, mouse);
        }
    } else if state.help_open {
        draw_help_overlay(d);
    } else if let Some((title, color)) = hovered_recent_footer(state, mouse) {
        let rendered = world::hsv_color(color.hue, color.sat, color.val);
        let hex = color_hex(rendered);
        let hsl = format!("H: {}  S: {}%  L: {}%", color.hue, color.sat, color.val);
        draw_button_tooltip(d, title, &[hex.as_str(), hsl.as_str()], mouse);
    } else if point_in(mouse, eraser_btn_rect()) {
        let (title, lines): (&str, &[&str]) = match state.tool {
            Tool::Paint => ("Paint", &["Click to cycle: Paint ->", "Eraser -> Move"]),
            Tool::Erase => ("Eraser", &["Revert a cell to its", "original color"]),
            Tool::Move => ("Move", &["Left-drag pans the camera", "instead of painting"]),
            Tool::Eyedropper => ("Paint", &["Click to return to", "the paint tool"]),
        };
        draw_button_tooltip(d, title, lines, mouse);
    } else if let Some((title, lines)) = hovered_button_tooltip(mouse) {
        draw_button_tooltip(d, title, lines, mouse);
    }
    if let Some(toast) = &state.toast {
        draw_toast(d, toast, info);
    }
    if let Some(secs) = info.rerank_secs {
        draw_rerank_banner(d, secs);
    }
    draw_like_anims(d, state);
    if state.hexa_success_open {
        draw_hexa_success_popup(d);
    }
}

/// Big, explicit acknowledgement for a successful HEXA. It deliberately
/// draws last, over both the map and HUD, and stays open until OK is clicked.
fn draw_hexa_success_popup(d: &mut impl RaylibDraw) {
    d.draw_rectangle_rec(Rectangle::new(0.0, 0.0, SCREEN_W, SCREEN_H), Color::new(5, 7, 13, 185));

    let panel = Rectangle::new(90.0, 90.0, 550.0, 550.0);
    d.draw_rectangle_rec(panel, Color::new(22, 25, 38, 250));
    d.draw_rectangle_lines_ex(panel, 3.0, Color::new(255, 220, 92, 255));
    d.draw_rectangle_lines_ex(Rectangle::new(panel.x + 8.0, panel.y + 8.0, panel.width - 16.0, panel.height - 16.0), 1.0, Color::new(255, 245, 190, 180));

    let centre = Vector2::new(SCREEN_W / 2.0, panel.y + 125.0);
    for i in 0..6 {
        let angle = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::FRAC_PI_3;
        let p = Vector2::new(centre.x + 78.0 * angle.cos(), centre.y + 78.0 * angle.sin());
        d.draw_poly(p, 6, 12.0, 0.0, Color::new(255, 210, 75, 210));
    }
    d.draw_poly(centre, 6, 29.0, 0.0, Color::new(255, 240, 170, 255));
    d.draw_poly_lines_ex(centre, 6, 29.0, 0.0, 2.0, Color::new(70, 48, 20, 255));

    d.draw_text("HEXA!", 270, 340, 62, Color::new(255, 232, 125, 255));
    d.draw_text("FORMATION COMPLETE", 216, 405, 24, Color::RAYWHITE);
    d.draw_text("Thank you for playing HEXEL.", 218, 457, 20, Color::new(255, 245, 205, 255));
    d.draw_text("You and your friends made the HEXA and pooled your colors.", 129, 488, 16, Color::new(220, 224, 236, 255));

    let ok = hexa_success_ok_rect();
    d.draw_rectangle_rounded(ok, 0.3, 8, Color::new(255, 220, 92, 255));
    d.draw_rectangle_rounded_lines(ok, 0.3, 8, Color::new(72, 52, 24, 255));
    d.draw_text("OK", ok.x as i32 + 58, ok.y as i32 + 10, 20, Color::new(38, 31, 22, 255));
}

/// F8/F9: own-island management panel (opened via the "My Isle" footer
/// button — a deliberate action, unlike the hover tooltip below) — creator
/// line, likes, age, and the link edit field/button. Full modal treatment
/// (backdrop, close button) since it has real form controls to interact
/// with, unlike the foreign-island case.
fn draw_island_popup(d: &mut impl RaylibDraw, state: &UiState, popup: &IslandInfo, info: &HudInfo) {
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

    // Author-requested: live-updates from the edit field as you type. Grey
    // while empty, blue (same link color as the foreign-island tooltip's
    // "Linked: ..." line) the instant it's a well-formed 7-digit itch.io
    // rate id — and therefore clickable — red while shorter or longer, so
    // it's clear clicking wouldn't currently go anywhere.
    let typed = state.link_edit_input.trim();
    let (link_label, link_color) = if typed.is_empty() {
        ("Your link: not set".to_string(), Color::LIGHTGRAY)
    } else if typed_link_id(state).is_some() {
        (format!("Your link: itch.io rate #{typed}"), Color::new(120, 180, 255, 255))
    } else {
        (format!("Your link: itch.io rate #{typed}"), Color::new(220, 90, 90, 255))
    };
    d.draw_text(&link_label, o.x as i32 + 20, o.y as i32 + 132, 16, link_color);
    d.draw_text("Set your itch.io rate id:", o.x as i32 + 20, o.y as i32 + 162, 14, Color::LIGHTGRAY);

    // Author-requested: no Set button — typing submits automatically (see
    // `handle_input`), so the field just fills the row.
    let field = link_edit_rect();
    d.draw_rectangle_rec(field, Color::new(28, 28, 34, 255));
    d.draw_rectangle_lines_ex(field, 1.0, if state.link_edit_focused { Color::GOLD } else { Color::new(90, 90, 96, 255) });
    let shown = if state.link_edit_input.is_empty() && !state.link_edit_focused { "e.g. 123456" } else { &state.link_edit_input };
    d.draw_text(shown, field.x as i32 + 6, field.y as i32 + 7, 14, Color::RAYWHITE);

    // Author-requested: pin the border to the current brush color, or toggle
    // it shown/hidden — two separate actions now, so un-hiding never
    // silently repins the color. "Set border to current color" previews the
    // change as [current border] -> [current cursor] rather than a bare
    // label, so the player can see what they're about to commit to before
    // clicking. The toggle button's own label carries the state (no separate
    // status line), highlighted red while hidden, same "active state is a
    // color change" language as the footer's lock button.
    let setb = border_set_btn_rect();
    d.draw_rectangle_rec(setb, Color::new(40, 40, 48, 255));
    d.draw_text("Set border:", setb.x as i32 + 10, setb.y as i32 + 11, 14, Color::RAYWHITE);
    let swatch = 16.0;
    let sw1 = Rectangle::new(setb.x + 168.0, setb.y + (setb.height - swatch) / 2.0, swatch, swatch);
    d.draw_rectangle_rec(sw1, popup.border_color);
    d.draw_rectangle_lines_ex(sw1, 1.0, Color::new(90, 90, 96, 255));
    d.draw_text("->", sw1.x as i32 + 20, setb.y as i32 + 11, 14, Color::LIGHTGRAY);
    let cursor_color = world::hsv_color(info.brush.0, info.brush.1, info.brush.2);
    let sw2 = Rectangle::new(sw1.x + 40.0, sw1.y, swatch, swatch);
    d.draw_rectangle_rec(sw2, cursor_color);
    d.draw_rectangle_lines_ex(sw2, 1.0, Color::new(90, 90, 96, 255));

    let tgb = border_toggle_btn_rect();
    d.draw_rectangle_rec(tgb, if popup.border_hidden { Color::new(120, 60, 60, 255) } else { Color::new(40, 40, 48, 255) });
    let toggle_label = if popup.border_hidden { "Border: Hidden" } else { "Border: Shown" };
    d.draw_text(toggle_label, tgb.x as i32 + 24, tgb.y as i32 + 10, 14, Color::RAYWHITE);
}

const TOOLTIP_W: f32 = 220.0;
const TOOLTIP_PAD: f32 = 8.0;
const TOOLTIP_LINE_H: f32 = 18.0;
const BTN_TOOLTIP_W: f32 = 210.0;

/// Author-requested: hovering a footer button shows a title + short
/// description — mainly for the icon-only Eraser/Lock buttons, which
/// otherwise carry no on-screen label at all. The name field is
/// deliberately excluded (self-explanatory as a text input).
const BUTTON_TOOLTIPS: &[(fn() -> Rectangle, &str, &[&str])] = &[
    (center_btn_rect, "Isle Centre", &["Go back to your island"]),
    (world_centre_btn_rect, "World Centre", &["Zoom out to see", "the whole world"]),
    (inventory_btn_rect, "Colors", &["Inventory: stores every", "color you've discovered"]),
    (recent_btn_rect, "Recent colors", &["Open your raw HSL", "color history"]),
    (lock_btn_rect, "Lock", &["Blocks cursor merging", "and central HEXA"]),
    (brush_btn_rect, "Eyedropper", &["Click or tap, then select", "a painted tile", "Merging is disabled while active"]),
    (account_btn_rect, "Account", &["Copy or import your ID,", "reset your account"]),
    (
        footer_link_edit_rect,
        "Link your island",
        &["Paste your raylib gamejam", "submission's rate id here.", "Other players clicking your", "island get sent to that page."],
    ),
    (my_island_btn_rect, "My Isle", &["Get info on your island", "and set your project link"]),
    (export_btn_rect, "Export", &["Save a shareable image", "of your island"]),
];

fn hovered_button_tooltip(mouse: Vector2) -> Option<(&'static str, &'static [&'static str])> {
    BUTTON_TOOLTIPS.iter().find(|&&(rect_fn, _, _)| point_in(mouse, rect_fn())).map(|&(_, title, lines)| (title, lines))
}

/// Author-requested: the footer's last-3 swatches (`UiState::last3`, most-
/// recently-used first, see its doc comment) get their own hover title —
/// "Last color used" for the newest, "Last last ..." for the one before,
/// "Last last last ..." for the oldest of the three — instead of being
/// silently excluded like the name field.
const LAST3_TITLES: [&str; 3] = ["Last color used", "Last last color used", "Last last last color used"];

fn hovered_recent_footer(state: &UiState, mouse: Vector2) -> Option<(&'static str, RecentColor)> {
    LAST3_TITLES
        .iter()
        .zip(state.recent_colors.iter().take(3))
        .enumerate()
        .find_map(|(i, (&title, &color))| point_in(mouse, last3_rect(i)).then_some((title, color)))
}

/// Same tooltip visual language as `draw_island_tooltip` (small, glued near
/// the cursor, flipped near screen edges) but for a fixed title + a couple
/// of description lines instead of live island data.
fn draw_button_tooltip(d: &mut impl RaylibDraw, title: &str, lines: &[&str], mouse: Vector2) {
    let height = TOOLTIP_PAD * 2.0 + TOOLTIP_LINE_H * (1 + lines.len()) as f32;
    let mut x = mouse.x + 16.0;
    let mut y = mouse.y - height - 10.0;
    if x + BTN_TOOLTIP_W > SCREEN_W {
        x = mouse.x - BTN_TOOLTIP_W - 10.0;
    }
    if y < 0.0 {
        y = mouse.y + 16.0;
    }
    let rect = Rectangle::new(x, y, BTN_TOOLTIP_W, height);
    d.draw_rectangle_rec(rect, Color::new(20, 20, 26, 235));
    d.draw_rectangle_lines_ex(rect, 1.0, Color::new(120, 120, 130, 200));

    let tx = rect.x as i32 + TOOLTIP_PAD as i32;
    let mut ty = rect.y as i32 + TOOLTIP_PAD as i32;
    d.draw_text(title, tx, ty, 15, Color::RAYWHITE);
    ty += TOOLTIP_LINE_H as i32;
    for line in lines {
        d.draw_text(line, tx, ty, 13, Color::LIGHTGRAY);
        ty += TOOLTIP_LINE_H as i32;
    }
}

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

/// Author-requested: a floating heart pop where a double-click like/unlike
/// landed, since that gesture (unlike the popup button) has no other visible
/// feedback — filled red heart for a like, empty grey outline heart for an
/// unlike (matching `world::draw_heart`'s filled/outline convention used
/// elsewhere for the like state). Grows slightly and fades out over
/// `LIKE_ANIM_DURATION`; screen-space, drawn over everything else.
fn draw_like_anims(d: &mut impl RaylibDraw, state: &UiState) {
    for anim in &state.like_anims {
        let t = (anim.started_at.elapsed().as_secs_f32() / LIKE_ANIM_DURATION.as_secs_f32()).clamp(0.0, 1.0);
        let alpha = ((1.0 - t) * 255.0) as u8;
        let rise = t * 26.0;
        let size = 20.0 + t * 10.0;
        let cy = anim.pos.y - rise;
        let color = if anim.liked { Color::new(230, 70, 90, alpha) } else { Color::new(160, 160, 168, alpha) };
        world::draw_heart(d, Vector2::new(anim.pos.x, cy), size, anim.liked, color);
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
/// `TOAST_DURATION` (the "new color" feedback from a merge). Author-
/// requested follow-up: when `toast.merge_from` is set (a cursor-merge, not
/// a tile-merge/eyedrop), also renders the two PRE-merge swatches as
/// "[mine] + [his] = [new]" before the flash swatch, instead of just the
/// result alone — widened and re-centered on the same axis so it still sits
/// under the header symmetrically.
fn draw_toast(d: &mut impl RaylibDraw, toast: &Toast, info: &HudInfo) {
    let frac = 1.0 - (toast.shown_at.elapsed().as_secs_f32() / TOAST_DURATION.as_secs_f32()).clamp(0.0, 1.0);
    let alpha = (frac * 235.0) as u8;
    let content_w = toast.text.chars().count() as f32 * 8.0
        + if toast.hue.is_some() { 42.0 } else { 20.0 }
        + if toast.merge_from.is_some() { 80.0 } else { 0.0 };
    let bar_w = content_w.clamp(if toast.merge_from.is_some() { 430.0 } else { 360.0 }, 680.0);
    let bar = Rectangle::new(360.0 - bar_w / 2.0, HEADER_H + 10.0, bar_w, 34.0);
    d.draw_rectangle_rec(bar, Color::new(24, 24, 30, alpha));
    d.draw_rectangle_lines_ex(bar, 1.0, Color::new(255, 215, 0, alpha));

    let mut x = bar.x + 6.0;
    if let Some((mine, his)) = toast.merge_from {
        let sw = 18.0;
        let y = bar.y + (bar.height - sw) / 2.0;
        let mut mine_c = canonical_color(mine);
        mine_c.a = alpha;
        let mut his_c = canonical_color(his);
        his_c.a = alpha;
        let glyph = Color::new(200, 200, 200, alpha);
        d.draw_rectangle_rec(Rectangle::new(x, y, sw, sw), mine_c);
        x += sw + 4.0;
        d.draw_text("+", x as i32, bar.y as i32 + 9, 14, glyph);
        x += 12.0;
        d.draw_rectangle_rec(Rectangle::new(x, y, sw, sw), his_c);
        x += sw + 4.0;
        d.draw_text("=", x as i32, bar.y as i32 + 9, 14, glyph);
        x += 16.0;
    }
    let text_x = if let Some(hue) = toast.hue {
        let swatch = Rectangle::new(x, bar.y + 6.0, 22.0, 22.0);
        let mut flash = world::hsv_color(hue, info.sat_cap, 90);
        flash.a = alpha;
        d.draw_rectangle_rec(swatch, flash);
        (x + 30.0) as i32
    } else {
        x as i32 + 4
    };
    d.draw_text(&toast.text, text_x, bar.y as i32 + 9, 14, Color::new(255, 255, 255, alpha));
}

fn draw_header(d: &mut impl RaylibDraw, info: &HudInfo) {
    d.draw_rectangle_rec(Rectangle::new(0.0, 0.0, SCREEN_W, HEADER_H), Color::new(10, 10, 14, 235));
    // Author-requested: the short identity hex used to lead this line, but
    // it's already reachable via the Account overlay ("Signed in as ..."),
    // so the header itself only needs the level/xp readout.
    d.draw_text(&format!("Lv{}  {}xp", info.level, info.xp), 10, 6, 16, Color::RAYWHITE);
    // Author-requested: sits in the gap right after the level/xp readout
    // (freed up by removing the web-only "ws: ..." debug line that used to
    // live here).
    d.draw_text("PRESS ESC for help", 110, 8, 12, Color::GRAY);
    d.draw_text("hexel", 338, 6, 16, Color::RAYWHITE);
    // Author-requested: camera world position, two stacked lines in the gap
    // between the wordmark and the online count.
    d.draw_text(&format!("X: {}", info.camera_target.x.round() as i32), 260, 2, 11, Color::LIGHTGRAY);
    d.draw_text(&format!("Y: {}", info.camera_target.y.round() as i32), 260, 14, 11, Color::LIGHTGRAY);
    // Fixed-position right-side label rather than measuring text width —
    // the draw handle has no default-font `measure_text` (that's only on
    // `RaylibHandle`, unavailable once `begin_drawing` hands out its borrow).
    // Shifted left from 548 to make room for the Account button now living
    // in the header (see `account_btn_rect`).
    d.draw_text(
        &format!("{} / {} online", info.online, info.total),
        430,
        6,
        14,
        Color::LIGHTGRAY,
    );
    // Author-requested: moved out of the footer, sitting directly left of
    // the Export button.
    let ab = account_btn_rect();
    d.draw_rectangle_rec(ab, Color::new(40, 40, 48, 255));
    d.draw_text("Account", ab.x as i32 + 8, ab.y as i32 + 6, 10, Color::RAYWHITE);

    let export = export_btn_rect();
    d.draw_rectangle_rec(export, Color::new(40, 40, 48, 255));
    draw_export_icon(d, export);
}

/// Clean, HUD-free branding added over the centred island for the exported
/// image. The map itself is rendered by the caller with the camera snapped to
/// the player's island; keeping the card here makes native and web exports
/// visually identical.
pub fn draw_export_frame(d: &mut impl RaylibDraw, name: &str) {
    let display_name = if name.trim().is_empty() { "My" } else { name.trim() };

    d.draw_rectangle_gradient_v(0, 0, SCREEN_W as i32, 88, Color::new(8, 10, 16, 245), Color::new(8, 10, 16, 0));
    d.draw_rectangle_gradient_v(
        0,
        (SCREEN_H - 72.0) as i32,
        SCREEN_W as i32,
        72,
        Color::new(8, 10, 16, 0),
        Color::new(8, 10, 16, 245),
    );
    d.draw_text("hexel", 24, 18, 24, Color::RAYWHITE);
    d.draw_text(&format!("{}'s island", display_name), 24, 47, 22, Color::new(210, 214, 224, 255));
    d.draw_text("https://raylib.mister-esman.uk", 168, 680, 16, Color::RAYWHITE);
}

fn draw_footer(d: &mut impl RaylibDraw, state: &UiState, info: &HudInfo) {
    d.draw_rectangle_rec(footer_bg(), Color::new(10, 10, 14, 235));

    let cb = center_btn_rect();
    d.draw_rectangle_rec(cb, Color::new(40, 40, 48, 255));
    draw_locate_icon(d, cb);

    let wcb = world_centre_btn_rect();
    d.draw_rectangle_rec(wcb, Color::new(40, 40, 48, 255));
    draw_globe_icon(d, wcb);

    for (i, &color) in state.recent_colors.iter().take(3).enumerate() {
        let r = last3_rect(i);
        d.draw_rectangle_rec(r, world::hsv_color(color.hue, color.sat, color.val));
        d.draw_rectangle_lines_ex(r, 1.0, Color::new(200, 200, 200, 180));
    }

    let ib = inventory_btn_rect();
    d.draw_rectangle_rec(ib, Color::new(40, 40, 48, 255));
    draw_colors_label(d, ib);

    // Author-requested: icon-only, left of the name field. The icon itself
    // shows which TOOL is active (pencil = painting, eraser = erasing, the
    // F13 follow-up's 4-arrow glyph = moving) rather than a static "eraser"
    // glyph that only ever meant "click to erase" — a colored border is the
    // active-state signal instead of a solid fill, so it reads apart from
    // the Lock button's fill-based signal right next to it.
    let eb = eraser_btn_rect();
    d.draw_rectangle_rec(eb, Color::new(40, 40, 48, 255));
    match state.tool {
        Tool::Paint => draw_pencil_icon(d, eb),
        Tool::Erase => {
            d.draw_rectangle_lines_ex(eb, 2.0, Color::new(220, 70, 70, 255));
            draw_eraser_icon(d, eb);
        }
        Tool::Move => {
            d.draw_rectangle_lines_ex(eb, 2.0, Color::new(90, 160, 230, 255));
            draw_move_icon(d, eb);
        }
        Tool::Eyedropper => draw_pencil_icon(d, eb),
    }

    let lb = lock_btn_rect();
    d.draw_rectangle_rec(lb, if info.locked { Color::new(120, 60, 60, 255) } else { Color::new(40, 40, 48, 255) });
    draw_lock_icon(d, lb, info.locked);

    let rb = recent_btn_rect();
    d.draw_rectangle_rec(rb, Color::new(40, 40, 48, 255));
    d.draw_text("+", rb.x as i32 + 9, rb.y as i32 + 3, 24, Color::RAYWHITE);

    let nf = name_field_rect();
    d.draw_rectangle_rec(nf, Color::new(28, 28, 34, 255));
    d.draw_rectangle_lines_ex(nf, 1.0, if state.name_focused { Color::GOLD } else { Color::new(90, 90, 96, 255) });
    let label = if state.name_input.is_empty() && !state.name_focused { "name..." } else { &state.name_input };
    d.draw_text(label, nf.x as i32 + 6, nf.y as i32 + 7, 14, Color::RAYWHITE);

    // Eyedropper tool. The tip droplet is tinted with the live brush color;
    // a gold border marks the active sampling state.
    let bb = brush_btn_rect();
    d.draw_rectangle_rec(bb, Color::new(40, 40, 48, 255));
    if state.tool == Tool::Eyedropper {
        d.draw_rectangle_lines_ex(bb, 2.0, Color::GOLD);
    }
    draw_brush_icon(d, bb, world::hsv_color(info.brush.0, info.brush.1, info.brush.2));

    // Author-requested: footer quick-access rate-id field, left of My Isle
    // (see `footer_link_edit_rect`'s doc comment) — same colored-by-validity
    // convention as the popup's own "Your link" row (`typed_link_id`).
    let fl = footer_link_edit_rect();
    d.draw_rectangle_rec(fl, Color::new(28, 28, 34, 255));
    let fl_border = if state.link_edit_focused {
        Color::GOLD
    } else if typed_link_id(state).is_some() {
        Color::new(120, 180, 255, 255)
    } else {
        Color::new(90, 90, 96, 255)
    };
    d.draw_rectangle_lines_ex(fl, 1.0, fl_border);
    let fl_shown = if state.link_edit_input.is_empty() && !state.link_edit_focused { "id" } else { &state.link_edit_input };
    d.draw_text(fl_shown, fl.x as i32 + 6, fl.y as i32 + 8, 12, Color::RAYWHITE);

    let mb = my_island_btn_rect();
    d.draw_rectangle_rec(mb, Color::new(40, 40, 48, 255));
    d.draw_text("My Isle", mb.x as i32 + 6, mb.y as i32 + 9, 10, Color::RAYWHITE);
}

/// Author-requested: each letter of the footer's "Colors" button in its own
/// hue, evenly spread around the wheel at the canonical reference sat/val
/// (`default_sat`/`DEFAULT_VAL`) — a little rainbow that previews the jam's
/// theme (unlocked colors) right on the button that opens the Inventory.
const COLORS_LABEL: &str = "C o l o r s";
const COLORS_LABEL_SIZE: i32 = 14;

/// Per-letter x offsets for `COLORS_LABEL` (plus the string's total measured
/// width, for centering), from the default font's actual glyph widths — a
/// fixed per-letter step looked evenly spaced for most letters but left a
/// visible gap after the narrow "l" (author-caught: rendered as "Col ors").
/// Computed once (raylib's `measure_text` needs a live `RaylibHandle`, only
/// available in `handle_input`, not here) and cached for `draw_colors_label`,
/// which runs later during `begin_drawing`.
static COLORS_LETTER_X: OnceLock<(Vec<i32>, i32)> = OnceLock::new();

fn colors_letter_offsets(rl: &RaylibHandle) -> &'static (Vec<i32>, i32) {
    COLORS_LETTER_X.get_or_init(|| {
        let mut x = 0;
        let mut offsets = Vec::with_capacity(COLORS_LABEL.len());
        for ch in COLORS_LABEL.chars() {
            offsets.push(x);
            x += rl.measure_text(&ch.to_string(), COLORS_LABEL_SIZE);
        }
        (offsets, x)
    })
}

fn draw_colors_label(d: &mut impl RaylibDraw, r: Rectangle) {
    let n = COLORS_LABEL.len() as u16;
    let cached = COLORS_LETTER_X.get();
    let offsets = cached.map(|(o, _)| o.as_slice());
    let total_w = cached.map_or(n as i32 * 9, |(_, w)| *w);
    let start_x = r.x + (r.width - total_w as f32) / 2.0;
    for (i, ch) in COLORS_LABEL.chars().enumerate() {
        let hue = (i as u16) * 360 / n;
        let color = canonical_color(hue);
        let dx = offsets.and_then(|o| o.get(i)).copied().unwrap_or(i as i32 * 9);
        d.draw_text(&ch.to_string(), (start_x + dx as f32) as i32, r.y as i32 + 7, COLORS_LABEL_SIZE, color);
    }
}

/// Author-requested: geolocation-style "recenter" icon for the Center
/// button — a ring with a filled center dot and four short compass ticks
/// poking out past the ring, the standard "locate me" glyph from map apps.
fn draw_locate_icon(d: &mut impl RaylibDraw, r: Rectangle) {
    let cx = r.x + r.width / 2.0;
    let cy = r.y + r.height / 2.0;
    let icon_color = Color::new(235, 235, 240, 255);
    let ring_r = 6.0;
    d.draw_ring(Vector2::new(cx, cy), ring_r - 1.5, ring_r, 0.0, 360.0, 24, icon_color);
    d.draw_circle(cx as i32, cy as i32, 2.0, icon_color);
    let gap = 1.0;
    let tick = 3.0;
    d.draw_line_ex(Vector2::new(cx, cy - ring_r - gap), Vector2::new(cx, cy - ring_r - gap - tick), 2.0, icon_color);
    d.draw_line_ex(Vector2::new(cx, cy + ring_r + gap), Vector2::new(cx, cy + ring_r + gap + tick), 2.0, icon_color);
    d.draw_line_ex(Vector2::new(cx - ring_r - gap, cy), Vector2::new(cx - ring_r - gap - tick, cy), 2.0, icon_color);
    d.draw_line_ex(Vector2::new(cx + ring_r + gap, cy), Vector2::new(cx + ring_r + gap + tick, cy), 2.0, icon_color);
}

/// World Centre glyph: a globe (ring plus a meridian/equator cross), distinct
/// from the locate-me ring-and-ticks used for Isle Centre right next to it.
fn draw_globe_icon(d: &mut impl RaylibDraw, r: Rectangle) {
    let cx = r.x + r.width / 2.0;
    let cy = r.y + r.height / 2.0;
    let icon_color = Color::new(235, 235, 240, 255);
    let ring_r = 7.0;
    d.draw_ring(Vector2::new(cx, cy), ring_r - 1.3, ring_r, 0.0, 360.0, 24, icon_color);
    d.draw_line_ex(Vector2::new(cx - ring_r, cy), Vector2::new(cx + ring_r, cy), 1.0, icon_color);
    d.draw_ellipse_lines(cx as i32, cy as i32, ring_r * 0.45, ring_r, icon_color);
}

/// Export/share glyph: an upward arrow leaving a small tray. This avoids a
/// text label in the narrow top-right header while still reading clearly as
/// "save this image".
fn draw_export_icon(d: &mut impl RaylibDraw, r: Rectangle) {
    let color = Color::new(235, 235, 240, 255);
    let cx = r.x + r.width / 2.0;
    d.draw_line_ex(Vector2::new(cx, r.y + 14.0), Vector2::new(cx, r.y + 5.0), 2.0, color);
    d.draw_triangle(
        Vector2::new(cx, r.y + 3.0),
        Vector2::new(cx - 4.0, r.y + 8.0),
        Vector2::new(cx + 4.0, r.y + 8.0),
        color,
    );
    d.draw_rectangle_lines((r.x + 6.0) as i32, (r.y + 14.0) as i32, 16, 5, color);
}

/// Default paint-mode icon for the paint/erase/move
/// tool button — a pencil with a pink eraser-band cap and a graphite tip,
/// swapped for `draw_eraser_icon`/`draw_move_icon` while `state.tool` is
/// `Erase`/`Move` (see `draw_footer`), so the button always shows which
/// tool is currently active.
///
/// Author-caught, twice: two earlier cuts of this used a diagonal
/// (rotated-quad-via-triangles) construction that kept rendering invisible
/// in practice despite hand-verified, non-degenerate geometry — widening it
/// and adding a stroke didn't help either time, which pointed at the custom
/// rotation math itself (untested anywhere else in this codebase) rather
/// than at sizing/contrast. Rebuilt axis-aligned instead, reusing the exact
/// same primitive calls `draw_eraser_icon` below already uses successfully
/// (`draw_rectangle_rounded`, `draw_rectangle_rec`, `draw_triangle`,
/// `draw_rectangle_lines`/`draw_triangle_lines`) — no custom geometry left
/// that isn't already proven to render correctly in this exact file.
fn draw_pencil_icon(d: &mut impl RaylibDraw, r: Rectangle) {
    let cap_w = 5.0;
    let body_w = 13.0;
    let tip_w = 7.0;
    let h = 9.0;
    let x = r.x + (r.width - (cap_w + body_w + tip_w)) / 2.0;
    let y = r.y + (r.height - h) / 2.0;

    let cap = Rectangle::new(x, y, cap_w, h);
    d.draw_rectangle_rounded(cap, 0.5, 4, Color::new(235, 120, 150, 255));

    let body = Rectangle::new(x + cap_w, y, body_w, h);
    d.draw_rectangle_rec(body, Color::new(235, 235, 240, 255));

    let tip_base_x = x + cap_w + body_w;
    let tip = [
        Vector2::new(tip_base_x, y),
        Vector2::new(tip_base_x, y + h),
        Vector2::new(tip_base_x + tip_w, y + h / 2.0),
    ];
    d.draw_triangle(tip[0], tip[1], tip[2], Color::new(190, 150, 100, 255));

    let outline = Color::new(40, 40, 48, 255);
    d.draw_rectangle_lines((x + cap_w) as i32, y as i32, body_w as i32, h as i32, outline);
    d.draw_triangle_lines(tip[0], tip[1], tip[2], outline);
    d.draw_rectangle_rounded_lines(cap, 0.5, 4, outline);
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

/// F13 follow-up: icon for the new Move tool state — the standard 4-way
/// "pan" glyph (a plus-shaped cross with an arrowhead on each end). Every
/// arrowhead is axis-aligned (up/down/left/right), so this needs no rotated
/// geometry — same constraint `draw_pencil_icon`'s doc comment explains.
fn draw_move_icon(d: &mut impl RaylibDraw, r: Rectangle) {
    let cx = r.x + r.width / 2.0;
    let cy = r.y + r.height / 2.0;
    let icon_color = Color::new(235, 235, 240, 255);
    let outline = Color::new(40, 40, 48, 255);
    let shaft_half_w = 1.5;
    let arm = 7.0;
    let head_w = 5.0;
    let head_l = 4.0;

    d.draw_rectangle_rec(Rectangle::new(cx - shaft_half_w, cy - arm, shaft_half_w * 2.0, arm * 2.0), icon_color);
    d.draw_rectangle_rec(Rectangle::new(cx - arm, cy - shaft_half_w, arm * 2.0, shaft_half_w * 2.0), icon_color);

    let heads = [
        // Up
        [Vector2::new(cx - head_w / 2.0, cy - arm), Vector2::new(cx + head_w / 2.0, cy - arm), Vector2::new(cx, cy - arm - head_l)],
        // Down
        [Vector2::new(cx - head_w / 2.0, cy + arm), Vector2::new(cx, cy + arm + head_l), Vector2::new(cx + head_w / 2.0, cy + arm)],
        // Leftnex
        [Vector2::new(cx - arm, cy - head_w / 2.0), Vector2::new(cx - arm - head_l, cy), Vector2::new(cx - arm, cy + head_w / 2.0)],
        // Right
        [Vector2::new(cx + arm, cy - head_w / 2.0), Vector2::new(cx + arm, cy + head_w / 2.0), Vector2::new(cx + arm + head_l, cy)],
    ];
    for h in heads {
        d.draw_triangle(h[0], h[1], h[2], icon_color);
        d.draw_triangle_lines(h[0], h[1], h[2], outline);
    }
    d.draw_circle(cx as i32, cy as i32, 2.0, icon_color);
    d.draw_circle_lines(cx as i32, cy as i32, 2.0, outline);
}

/// Paintbrush glyph with a color
/// droplet at the tip. Axis-aligned only — see `draw_pencil_icon`'s doc
/// comment above for why rotated custom geometry is avoided in this file.
fn draw_brush_icon(d: &mut impl RaylibDraw, r: Rectangle, tip_color: Color) {
    let handle_w = 8.0;
    let handle_h = 9.0;
    let ferrule_h = 4.0;
    let bristle_h = 6.0;
    let total_h = handle_h + ferrule_h + bristle_h;
    let x = r.x + (r.width - handle_w) / 2.0;
    let y = r.y + (r.height - total_h) / 2.0 - 1.0;
    let outline = Color::new(40, 40, 48, 255);

    let handle = Rectangle::new(x, y, handle_w, handle_h);
    d.draw_rectangle_rounded(handle, 0.3, 4, Color::new(200, 150, 90, 255));
    d.draw_rectangle_rounded_lines(handle, 0.3, 4, outline);

    let bristle_top_w = handle_w + 2.0;
    let ferrule = Rectangle::new(x - 1.0, y + handle_h, bristle_top_w, ferrule_h);
    d.draw_rectangle_rec(ferrule, Color::new(190, 190, 196, 255));
    d.draw_rectangle_lines(ferrule.x as i32, ferrule.y as i32, ferrule.width as i32, ferrule.height as i32, outline);

    let bristle_y = y + handle_h + ferrule_h;
    let tip_cx = x - 1.0 + bristle_top_w / 2.0;
    let tip = [
        Vector2::new(x - 1.0, bristle_y),
        Vector2::new(x - 1.0 + bristle_top_w, bristle_y),
        Vector2::new(tip_cx, bristle_y + bristle_h),
    ];
    // Match raylib's visible-face winding. The reverse order can be culled,
    // leaving the map visible through the bristle area and making the icon
    // look partially transparent.
    d.draw_triangle(tip[0], tip[2], tip[1], Color::new(60, 55, 50, 255));
    d.draw_triangle_lines(tip[0], tip[1], tip[2], outline);

    // Droplet: the caller's LIVE brush color — reinforces "pick up a color"
    // rather than reading as a generic paint tool.
    let drop_c = Vector2::new(tip_cx, bristle_y + bristle_h + 2.0);
    d.draw_circle_v(drop_c, 3.0, tip_color);
    d.draw_circle_lines(drop_c.x as i32, drop_c.y as i32, 3.0, outline);
}

/// World-cursor version of the footer eyedropper icon. The droplet is
/// anchored at `tip` and previews the painted color currently underneath.
pub fn draw_eyedropper_cursor(d: &mut impl RaylibDraw, tip: Vector2, preview: Color) {
    draw_brush_icon(d, Rectangle::new(tip.x - 15.0, tip.y - 25.5, 30.0, 30.0), preview);
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
/// F12 (extends F9.6 item 5's minimal keybindings-only version): explains
/// the actual theme mechanic — merging — above the controls list, since
/// "how do I even get new colors" was never spelled out anywhere in-game.
fn draw_help_overlay(d: &mut impl RaylibDraw) {
    d.draw_rectangle_rec(Rectangle::new(0.0, 0.0, SCREEN_W, SCREEN_H), Color::new(0, 0, 0, 140));

    let o = overlay_rect();
    d.draw_rectangle_rec(o, Color::new(24, 24, 30, 250));
    d.draw_rectangle_lines_ex(o, 2.0, Color::new(90, 90, 100, 255));
    d.draw_text("How to play", o.x as i32 + 20, o.y as i32 + 14, 18, Color::RAYWHITE);

    let close = overlay_close_rect();
    d.draw_rectangle_rec(close, Color::new(60, 40, 40, 255));
    d.draw_text("X", close.x as i32 + 11, close.y as i32 + 7, 16, Color::RAYWHITE);

    let section_color = Color::new(180, 200, 255, 255);
    let mut ty = o.y as i32 + 50;

    d.draw_text("Merging colors", o.x as i32 + 20, ty, 15, section_color);
    ty += 24;
    const MERGE_LINES: &[&str] = &[
        "Hues are your unlockable resource. You start with just one.",
        "Touch cursors with another player and you both blend into a",
        "  brand-new hue and you both add it to your collection.",
        "No other player around? Long-press any painted tile instead.",
        "You'll directly take that tile's color.",
        "Merging earns XP and XP raises how saturated your brush can go.",
    ];
    for line in MERGE_LINES {
        d.draw_text(line, o.x as i32 + 20, ty, 14, Color::RAYWHITE);
        ty += 22;
    }

    ty += 14;
    d.draw_text("Controls", o.x as i32 + 20, ty, 15, section_color);
    ty += 24;
    const CONTROL_LINES: &[&str] = &[
        "Left-drag on your island or the margin: paint",
        "X or the tool button: cycle Paint / Eraser / Move",
        "Move tool: left-drag pans instead of painting",
        "Eyedropper button, then click/tap a painted tile",
        "Middle-click is the eyedropper shortcut",
        "Long-press a foreign tile: merge/take its color",
        "Double-click/-tap a foreign island: like / unlike",
        "Hover (or tap) a foreign island: info",
        "WASD / arrow keys: pan     Q / E: zoom",
        "Right-drag, middle-drag, or Shift+left-drag: pan",
        "Mouse wheel / pinch: zoom",
        "Escape: close this / any open panel",
    ];
    for line in CONTROL_LINES {
        d.draw_text(line, o.x as i32 + 20, ty, 15, Color::RAYWHITE);
        ty += 26;
    }
}

/// Author-requested: selection marker for an inventory tile — four small
/// white "L" corner brackets (camera-reticle style) instead of a colored
/// border, since the border is now spoken for by `canonical_color`. Hover
/// uses the same shape at reduced alpha (pass a translucent/gray `color`).
fn draw_crosshair(d: &mut impl RaylibDraw, r: Rectangle, color: Color) {
    let len = 8.0;
    let thick = 2.0;
    let corners = [
        (Vector2::new(r.x, r.y), Vector2::new(1.0, 1.0)),
        (Vector2::new(r.x + r.width, r.y), Vector2::new(-1.0, 1.0)),
        (Vector2::new(r.x, r.y + r.height), Vector2::new(1.0, -1.0)),
        (Vector2::new(r.x + r.width, r.y + r.height), Vector2::new(-1.0, -1.0)),
    ];
    for (corner, dir) in corners {
        d.draw_line_ex(corner, Vector2::new(corner.x + len * dir.x, corner.y), thick, color);
        d.draw_line_ex(corner, Vector2::new(corner.x, corner.y + len * dir.y), thick, color);
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
    // pops up above whichever swatch the mouse is currently over. Author-
    // requested follow-up: the label also names the raw hue degree (the
    // actual unlockable resource, decision 7) alongside the hex code, since
    // two close swatches can look identical at a glance otherwise.
    let mut hovered_hex: Option<(Rectangle, String)> = None;
    for (i, &hue) in sorted_hues(info).iter().enumerate() {
        let r = swatch_rect(i);
        // Author-requested: the fill is the tuned "what we'll draw with"
        // color (per-hue sat/val, live for the selected swatch), while the
        // border is always the untouched canonical hue — comparing the two
        // is how the player sees what a tuned swatch actually shifted from.
        let color = swatch_color(hue, state, info);
        d.draw_rectangle_rec(r, color);
        d.draw_rectangle_lines_ex(r, 5.0, canonical_color(hue));
        // Compare against the Hue slider's anchor, not the live (possibly
        // nudged) brush hue — otherwise dragging the slider away from 0
        // makes every swatch look unselected even though you're still
        // fine-tuning the same one.
        let selected = hue == state.base_hue;
        let hovered = point_in(mouse, r);
        if selected {
            draw_crosshair(d, r, Color::WHITE);
        } else if hovered {
            draw_crosshair(d, r, Color::new(255, 255, 255, 110));
        }
        if hovered {
            hovered_hex = Some((r, format!("{}  H:{hue}", color_hex(color))));
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
    draw_hue_slider(d, hue_slider_rect(), offset, tol, state.base_hue, info.brush.1, info.brush.2);
    let sat_track = sat_slider_rect();
    draw_slider_capped(
        d,
        sat_track,
        info.brush.1,
        100,
        info.sat_cap,
        world::hsv_color(info.brush.0, 0, info.brush.2),
        world::hsv_color(info.brush.0, 100, info.brush.2),
        &format!("Saturation ({})", info.brush.1),
    );
    draw_slider(
        d,
        val_slider_rect(),
        info.brush.2,
        100,
        world::hsv_color(info.brush.0, info.brush.1, 0),
        world::hsv_color(info.brush.0, info.brush.1, 100),
        &format!("Lightness ({})", info.brush.2),
    );

    // Hovering the not-yet-unlocked tail of the Saturation slider explains
    // why it won't drag past `sat_cap`, instead of just silently refusing.
    if info.sat_cap < 100 && point_in(mouse, slider_hit(sat_track)) {
        let cap_x = sat_track.x + sat_track.width * (info.sat_cap as f32 / 100.0);
        if mouse.x > cap_x {
            draw_button_tooltip(
                d,
                "Saturation locked",
                &["You need more XP to", "unlock more saturation"],
                mouse,
            );
        }
    }
}

fn draw_recent_overlay(d: &mut impl RaylibDraw, state: &UiState, mouse: Vector2) {
    d.draw_rectangle_rec(Rectangle::new(0.0, 0.0, SCREEN_W, SCREEN_H), Color::new(0, 0, 0, 140));
    let o = overlay_rect();
    d.draw_rectangle_rec(o, Color::new(24, 24, 30, 250));
    d.draw_rectangle_lines_ex(o, 2.0, Color::new(90, 90, 100, 255));
    d.draw_text("Recent colors", o.x as i32 + 20, o.y as i32 + 14, 18, Color::RAYWHITE);

    let close = overlay_close_rect();
    d.draw_rectangle_rec(close, Color::new(60, 40, 40, 255));
    d.draw_text("X", close.x as i32 + 11, close.y as i32 + 7, 16, Color::RAYWHITE);

    let mut hovered = None;
    for (i, &color) in state.recent_colors.iter().enumerate() {
        let r = recent_swatch_rect(i);
        d.draw_rectangle_rec(r, world::hsv_color(color.hue, color.sat, color.val));
        d.draw_rectangle_lines_ex(r, 1.0, Color::new(200, 200, 210, 180));
        if point_in(mouse, r) {
            draw_crosshair(d, r, Color::WHITE);
            hovered = Some(color);
        }
    }
    if let Some(color) = hovered {
        let rendered = world::hsv_color(color.hue, color.sat, color.val);
        let hex = color_hex(rendered);
        let hsl = format!("H: {}  S: {}%  L: {}%", color.hue, color.sat, color.val);
        draw_button_tooltip(d, "Recent color", &[hex.as_str(), hsl.as_str()], mouse);
    }
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
fn draw_hue_slider(
    d: &mut impl RaylibDraw,
    track: Rectangle,
    offset: i32,
    tol: i32,
    base_hue: u16,
    sat: u8,
    val: u8,
) {
    let label = if offset == 0 { "Hue (0)".to_string() } else { format!("Hue ({offset:+})") };
    d.draw_text(&label, track.x as i32, track.y as i32 - 18, 14, Color::LIGHTGRAY);
    // Left/right edges of the track are the ±tol extremes of the window, so the
    // gradient previews what dragging to either end would actually look like.
    let lo_hue = (base_hue as i32 - tol).rem_euclid(360) as u16;
    let hi_hue = (base_hue as i32 + tol).rem_euclid(360) as u16;
    d.draw_rectangle_gradient_h(
        track.x as i32,
        track.y as i32,
        track.width as i32,
        track.height as i32,
        world::hsv_color(lo_hue, sat, val),
        world::hsv_color(hi_hue, sat, val),
    );
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

fn draw_slider(d: &mut impl RaylibDraw, track: Rectangle, value: u8, max: u8, lo: Color, hi: Color, label: &str) {
    draw_slider_capped(d, track, value, max, max, lo, hi, label);
}

/// Like `draw_slider`, but the track always spans `0..max` and the region
/// beyond `cap` (e.g. the not-yet-unlocked saturation range) is rendered
/// dimmed with a marker line, instead of just shrinking the whole track to
/// `0..cap` the way the plain slider does — so the player can see there's
/// more range to grow into as they level up. Background is a `lo`→`hi`
/// gradient (what this channel actually looks like end to end at the
/// current brush), same visual language as `draw_hue_slider`, instead of a
/// flat bar.
fn draw_slider_capped(
    d: &mut impl RaylibDraw,
    track: Rectangle,
    value: u8,
    max: u8,
    cap: u8,
    lo: Color,
    hi: Color,
    label: &str,
) {
    d.draw_text(label, track.x as i32, track.y as i32 - 18, 14, Color::LIGHTGRAY);
    d.draw_rectangle_gradient_h(track.x as i32, track.y as i32, track.width as i32, track.height as i32, lo, hi);
    if cap < max {
        let cap_frac = cap as f32 / max as f32;
        let locked = Rectangle::new(
            track.x + track.width * cap_frac,
            track.y,
            track.width * (1.0 - cap_frac),
            track.height,
        );
        d.draw_rectangle_rec(locked, Color::new(20, 20, 24, 190));
    }
    let frac = if max == 0 { 0.0 } else { value as f32 / max as f32 };
    let handle_x = track.x + track.width * frac;
    d.draw_circle(handle_x as i32, (track.y + track.height / 2.0) as i32, 9.0, Color::RAYWHITE);
    if cap < max {
        let cap_x = track.x + track.width * (cap as f32 / max as f32);
        d.draw_line_ex(
            Vector2::new(cap_x, track.y - 3.0),
            Vector2::new(cap_x, track.y + track.height + 3.0),
            2.0,
            Color::new(220, 180, 80, 255),
        );
    }
}

// --- Title screen: "hexel" hexagon logotype + animated Draw button --------
//
// The logotype spells the word out of hexagon cells on the game's own
// flat-top axial grid (`world::axial_to_world` geometry: x advances 1.5
// radii per column, odd columns sit half a row lower — which is what gives
// the `h`/`e` their rounded shoulders for free). Each glyph is a cell list
// of `(q, 2*v)` where `v = r + q/2` is the *visual* row; doubling keeps the
// odd columns' half-row offset integral in the const tables. Baseline sits
// at v=6, x-height letters top out around v=2.5–3, ascenders at v=0.

const TITLE_GLYPH_H: &[(i32, i32)] = &[
    (0, 0), (0, 2), (0, 4), (0, 6), (0, 8), (0, 10), (0, 12), // left stem (ascender)
    (1, 5),                                                   // shoulder
    (2, 6), (2, 8), (2, 10), (2, 12),                         // right stem
];
const TITLE_GLYPH_E: &[(i32, i32)] = &[
    (1, 5),                    // top cap
    (0, 6), (0, 8), (0, 10),   // left side
    (2, 6), (2, 8),            // right side, upper half
    (1, 9),                    // crossbar (counter above, mouth below-right)
    (1, 11), (2, 12),          // bottom sweep + tail
];
const TITLE_GLYPH_X: &[(i32, i32)] = &[
    (0, 6), (2, 6),   // top arms
    (1, 9),           // crossing
    (0, 12), (2, 12), // bottom arms
];
const TITLE_GLYPH_L: &[(i32, i32)] = &[(0, 0), (0, 2), (0, 4), (0, 6), (0, 8), (0, 10), (0, 12)];

/// The word: each glyph with its column offset (3-wide letters, 1-column
/// gaps, the final `l` is a single column — 17 columns total).
const TITLE_WORD: [(&[(i32, i32)], i32); 5] =
    [(TITLE_GLYPH_H, 0), (TITLE_GLYPH_E, 4), (TITLE_GLYPH_X, 8), (TITLE_GLYPH_E, 12), (TITLE_GLYPH_L, 16)];
const TITLE_COLS: i32 = 17;

/// Logo cell outer radius: 17 columns span `16*1.5 + 2` = 26 radii, so 23
/// keeps the wordmark just under 600 px wide inside the 720 px screen.
const TITLE_CELL_R: f32 = 23.0;
const TITLE_LOGO_CY: f32 = 250.0;
const TITLE_LABEL_SIZE: i32 = 30;

fn title_btn_rect() -> Rectangle {
    Rectangle::new((SCREEN_W - 190.0) / 2.0, 486.0, 190.0, 58.0)
}

/// Pixel width of the "Draw" label — same `OnceLock` warm-from-`handle_input`
/// trick as `colors_letter_offsets` (`measure_text` needs a live
/// `RaylibHandle`, which `draw`'s `impl RaylibDraw` doesn't provide).
static TITLE_LABEL_W: OnceLock<i32> = OnceLock::new();

fn title_label_width(rl: &RaylibHandle) -> i32 {
    *TITLE_LABEL_W.get_or_init(|| rl.measure_text("Draw", TITLE_LABEL_SIZE))
}

/// The "hexel" wordmark, centered on `center`. Cell hue sweeps the color
/// wheel across the word (the game *is* hue-merging) at the canonical
/// HSV rendering; `t` drives a gentle brightness shimmer that travels
/// along the word.
fn draw_hexel_logo(d: &mut impl RaylibDraw, center: Vector2, cell_r: f32, t: f32) {
    let sqrt3 = 3f32.sqrt();
    let cell_center = |gq: i32, v2: i32| {
        Vector2::new(
            center.x + 1.5 * cell_r * (gq as f32 - (TITLE_COLS - 1) as f32 / 2.0),
            center.y + sqrt3 * cell_r * (v2 as f32 / 2.0 - 3.0), // v=3 is the wordmark's vertical middle
        )
    };
    // Drop shadow as its own full pass so a cell's shadow never lands on a
    // neighboring cell's fill.
    for (glyph, off) in TITLE_WORD {
        for &(q, v2) in glyph {
            let mut c = cell_center(q + off, v2);
            c.x += cell_r * 0.10;
            c.y += cell_r * 0.28;
            world::draw_hex(d, c, cell_r * 0.94, Color::new(0, 0, 0, 110), None);
        }
    }
    for (glyph, off) in TITLE_WORD {
        for &(q, v2) in glyph {
            let gq = q + off;
            let hue = (330.0 * gq as f32 / (TITLE_COLS - 1) as f32) as u16;
            let val = 88.0 + 5.0 * (t * 2.2 - gq as f32 * 0.45).sin();
            let fill = world::hsv_color(hue, 70, val as u8);
            let line = world::hsv_color(hue, 70, (val * 0.55) as u8);
            world::draw_hex(d, cell_center(gq, v2), cell_r * 0.94, fill, Some(line));
        }
    }
}

/// Full title screen: a semi-transparent backdrop (the whole map stays
/// hinted behind — the clients hold the camera on the whole-world pose while
/// `title_active`), the wordmark, and the pulsing Draw button.
fn draw_title_screen(d: &mut impl RaylibDraw, state: &UiState, mouse: Vector2) {
    let t = state.title_started.elapsed().as_secs_f32();
    d.draw_rectangle_rec(Rectangle::new(0.0, 0.0, SCREEN_W, SCREEN_H), Color::new(8, 9, 14, 205));
    draw_hexel_logo(d, Vector2::new(SCREEN_W / 2.0, TITLE_LOGO_CY), TITLE_CELL_R, t);

    let base = title_btn_rect();
    let hovered = point_in(mouse, base);
    // Idle: soft breathing pulse to invite the click; hovered: settled,
    // slightly enlarged (a pulse under the cursor reads as jitter).
    let scale = if hovered { 1.07 } else { 1.0 + 0.03 * (t * 3.2).sin() };
    let r = Rectangle::new(
        base.x + base.width * (1.0 - scale) / 2.0,
        base.y + base.height * (1.0 - scale) / 2.0,
        base.width * scale,
        base.height * scale,
    );
    let fill = if hovered { Color::RAYWHITE } else { Color::new(228, 229, 235, 255) };
    d.draw_rectangle_rounded(r, 0.45, 8, fill);
    d.draw_rectangle_rounded_lines(r, 0.45, 8, Color::new(40, 40, 48, 255));

    // Warmed by `handle_input` every title frame; the fallback only covers
    // a draw happening before any input pass ever ran.
    let label_w = TITLE_LABEL_W.get().copied().unwrap_or(66);
    let x = r.x + (r.width - label_w as f32) / 2.0;
    let cy = r.y + r.height / 2.0;
    d.draw_text(
        "Draw",
        (x) as i32,
        (cy - TITLE_LABEL_SIZE as f32 / 2.0) as i32,
        TITLE_LABEL_SIZE,
        Color::new(20, 20, 26, 255),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eyedropper_restores_only_a_temporary_lock() {
        let mut state = UiState::new();
        assert!(state.arm_eyedropper(false));
        assert_eq!(state.tool, Tool::Eyedropper);
        assert!(state.finish_eyedropper());
        assert_eq!(state.tool, Tool::Paint);

        assert!(!state.arm_eyedropper(true));
        assert!(!state.finish_eyedropper());
    }

    #[test]
    fn recent_colors_keep_raw_hsl_dedupe_and_cap_at_grid_size() {
        let mut state = UiState::new();
        for hue in 0..105 {
            state.note_used_color(RecentColor { hue, sat: 40, val: 90 });
        }
        assert_eq!(state.recent_colors.len(), 99);
        assert_eq!(state.recent_colors.front().unwrap().hue, 104);
        assert_eq!(state.recent_colors.back().unwrap().hue, 6);

        let raw_variant = RecentColor { hue: 104, sat: 55, val: 72 };
        state.note_used_color(raw_variant);
        assert_eq!(state.recent_colors.len(), 99);
        assert_eq!(state.recent_colors.front(), Some(&raw_variant));
        state.note_used_color(raw_variant);
        assert_eq!(state.recent_colors.len(), 99);
    }
}
