//! Immediate-mode HUD: header, footer, and the inventory/color-picker
//! overlay. Raylib primitives only — no new dependencies. `main.rs` calls
//! `handle_input` (before `begin_drawing`, since text/mouse input needs
//! `&mut RaylibHandle`) then `draw` (during `begin_drawing`). Layout
//! constants here are the shared source of truth for the header/footer band
//! heights so the map viewport can size around them.

use raylib::prelude::*;
use spacetimedb_sdk::Identity;
use std::collections::VecDeque;

use crate::world;

pub const HEADER_H: f32 = 28.0;
pub const FOOTER_H: f32 = 44.0;
const SCREEN_W: f32 = 720.0;
const SCREEN_H: f32 = 720.0;

fn point_in(p: Vector2, r: Rectangle) -> bool {
    p.x >= r.x && p.x <= r.x + r.width && p.y >= r.y && p.y <= r.y + r.height
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Sat,
    Val,
}

pub struct UiState {
    pub name_input: String,
    name_loaded: bool,
    name_focused: bool,
    pub overlay_open: bool,
    pub last3: VecDeque<u16>,
    dragging: Drag,
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
        }
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
}

/// Snapshot of server-derived state the HUD needs to read this frame.
/// Passed to both `handle_input` (for cap/clamp logic) and `draw`.
pub struct HudInfo<'a> {
    pub me: Identity,
    pub level: u64,
    pub xp: u64,
    pub online: usize,
    pub total: usize,
    pub locked: bool,
    pub brush: (u16, u8, u8),
    pub sat_cap: u8,
    pub hues: &'a [u16],
}

#[derive(Default)]
pub struct Actions {
    pub set_brush: Option<(u16, u8, u8)>,
    pub set_name: Option<String>,
    pub set_lock: Option<bool>,
    pub center_camera: bool,
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

fn name_field_rect() -> Rectangle {
    Rectangle::new(250.0, SCREEN_H - FOOTER_H + 7.0, 200.0, 30.0)
}

fn lock_btn_rect() -> Rectangle {
    Rectangle::new(458.0, SCREEN_H - FOOTER_H + 7.0, 90.0, 30.0)
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

fn sat_slider_rect() -> Rectangle {
    let o = overlay_rect();
    Rectangle::new(o.x + 40.0, o.y + o.height - 110.0, 440.0, 16.0)
}

fn val_slider_rect() -> Rectangle {
    let o = overlay_rect();
    Rectangle::new(o.x + 40.0, o.y + o.height - 60.0, 440.0, 16.0)
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

/// The currently-selected hue is rendered at the ACTUAL brush sat/val, so
/// dragging the sliders visibly repaints it live; every other swatch is just
/// a "what you'd get" preview at the sat cap.
fn swatch_color(hue: u16, info: &HudInfo) -> Color {
    if hue == info.brush.0 {
        world::hsv_color(hue, info.brush.1, info.brush.2)
    } else {
        world::hsv_color(hue, info.sat_cap, 90)
    }
}

pub fn handle_input(rl: &mut RaylibHandle, state: &mut UiState, info: &HudInfo) -> Actions {
    let mut actions = Actions::default();
    let mouse = rl.get_mouse_position();
    let clicked = rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT);
    let held = rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT);
    let released = rl.is_mouse_button_released(MouseButton::MOUSE_BUTTON_LEFT);

    if clicked && point_in(mouse, center_btn_rect()) {
        actions.center_camera = true;
    }
    for (i, &hue) in state.last3.iter().enumerate() {
        if clicked && point_in(mouse, last3_rect(i)) {
            actions.set_brush = Some((hue, info.brush.1.min(info.sat_cap), info.brush.2));
        }
    }
    if clicked && point_in(mouse, inventory_btn_rect()) {
        state.overlay_open = !state.overlay_open;
    }
    if clicked && point_in(mouse, lock_btn_rect()) {
        actions.set_lock = Some(!info.locked);
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
    for (i, &hue) in info.hues.iter().enumerate() {
        if clicked && point_in(mouse, swatch_rect(i)) {
            actions.set_brush = Some((hue, info.brush.1.min(info.sat_cap), info.brush.2));
        }
    }

    let sat_hit = slider_hit(sat_slider_rect());
    let val_hit = slider_hit(val_slider_rect());
    if clicked {
        if point_in(mouse, sat_hit) {
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

pub fn draw(d: &mut impl RaylibDraw, state: &UiState, info: &HudInfo) {
    draw_header(d, info);
    draw_footer(d, state, info);
    if state.overlay_open {
        draw_overlay(d, info);
    }
}

fn draw_header(d: &mut impl RaylibDraw, info: &HudInfo) {
    d.draw_rectangle_rec(Rectangle::new(0.0, 0.0, SCREEN_W, HEADER_H), Color::new(10, 10, 14, 235));
    let id = info.me.to_hex().to_string();
    let short = &id[..8.min(id.len())];
    d.draw_text(
        &format!("{short}   Lv{}  {}xp", info.level, info.xp),
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

    let nf = name_field_rect();
    d.draw_rectangle_rec(nf, Color::new(28, 28, 34, 255));
    d.draw_rectangle_lines_ex(nf, 1.0, if state.name_focused { Color::GOLD } else { Color::new(90, 90, 96, 255) });
    let label = if state.name_input.is_empty() && !state.name_focused { "name..." } else { &state.name_input };
    d.draw_text(label, nf.x as i32 + 6, nf.y as i32 + 7, 14, Color::RAYWHITE);

    let lb = lock_btn_rect();
    d.draw_rectangle_rec(lb, if info.locked { Color::new(120, 60, 60, 255) } else { Color::new(40, 40, 48, 255) });
    d.draw_text(
        if info.locked { "Lock: on" } else { "Lock: off" },
        lb.x as i32 + 6,
        lb.y as i32 + 7,
        14,
        Color::RAYWHITE,
    );
}

fn draw_overlay(d: &mut impl RaylibDraw, info: &HudInfo) {
    // Dim the world behind the modal.
    d.draw_rectangle_rec(Rectangle::new(0.0, 0.0, SCREEN_W, SCREEN_H), Color::new(0, 0, 0, 140));

    let o = overlay_rect();
    d.draw_rectangle_rec(o, Color::new(24, 24, 30, 250));
    d.draw_rectangle_lines_ex(o, 2.0, Color::new(90, 90, 100, 255));
    d.draw_text("Inventory", o.x as i32 + 20, o.y as i32 + 14, 18, Color::RAYWHITE);

    let close = overlay_close_rect();
    d.draw_rectangle_rec(close, Color::new(60, 40, 40, 255));
    d.draw_text("X", close.x as i32 + 11, close.y as i32 + 7, 16, Color::RAYWHITE);

    for (i, &hue) in info.hues.iter().enumerate() {
        let r = swatch_rect(i);
        d.draw_rectangle_rec(r, swatch_color(hue, info));
        let selected = hue == info.brush.0;
        d.draw_rectangle_lines_ex(
            r,
            if selected { 3.0 } else { 1.0 },
            if selected { Color::GOLD } else { Color::new(200, 200, 200, 160) },
        );
    }

    draw_slider(d, sat_slider_rect(), info.brush.1, info.sat_cap, &format!("Saturation ({})", info.brush.1));
    draw_slider(d, val_slider_rect(), info.brush.2, 100, &format!("Value ({})", info.brush.2));
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
