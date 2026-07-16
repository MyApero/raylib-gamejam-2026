//! Read-only, client-side replay of the timestamped world snapshot.
//!
//! SpacetimeDB's commitlog can rebuild historical database states, but that
//! history is not exposed by the normal client subscription API. The public
//! `island`, `island_cell`, and `margin_cell` rows do carry creation/last-paint
//! timestamps, though, so both clients can still reveal the retained snapshot
//! in chronological order without a server migration.

use raylib::prelude::*;

pub const DEFAULT_DURATION_SECS: f32 = 30.0;
/// Replay alone may zoom farther out than normal gameplay so the retained
/// world cannot lose outer islands to the ordinary 0.25 camera floor.
pub const MIN_CAMERA_ZOOM: f32 = 0.01;
const MIN_DURATION_SECS: f32 = 0.1;
const MIN_SPEED: f32 = 0.01;
const MAX_SPEED: f32 = 102.4;

/// Parse `--replay`, `--replay=SECONDS`, or `HEXEL_REPLAY_SECONDS=SECONDS`.
///
/// A bare `--replay` uses 30 seconds. Returning an error here (rather than
/// silently accepting a typo) prevents a long-running capture from using an
/// accidental pace.
#[cfg(not(target_os = "emscripten"))]
pub fn requested_duration() -> Result<Option<f32>, String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--replay" {
            let seconds = args
                .next()
                .filter(|next| !next.starts_with('-'))
                .map(|value| parse_duration(&value))
                .transpose()?
                .unwrap_or(DEFAULT_DURATION_SECS);
            return Ok(Some(seconds));
        }
        if let Some(value) = arg.strip_prefix("--replay=") {
            return parse_duration(value).map(Some);
        }
    }

    std::env::var("HEXEL_REPLAY_SECONDS")
        .ok()
        .map(|value| parse_duration(&value))
        .transpose()
}

#[cfg(not(target_os = "emscripten"))]
fn parse_duration(value: &str) -> Result<f32, String> {
    let seconds = value
        .parse::<f32>()
        .map_err(|_| format!("invalid replay duration {value:?}; expected seconds, e.g. 30"))?;
    if !seconds.is_finite() || seconds < MIN_DURATION_SECS {
        return Err(format!(
            "replay duration must be at least {MIN_DURATION_SECS} seconds"
        ));
    }
    Ok(seconds)
}

#[derive(Debug, Clone)]
pub struct ReplayClock {
    start_micros: i64,
    end_micros: i64,
    duration_secs: f32,
    elapsed_secs: f32,
    speed: f32,
    paused: bool,
    /// Set while the progress slider is being dragged, so a drag that
    /// strays outside `progress_track_rect` vertically keeps tracking the
    /// mouse until release instead of dropping the scrub.
    scrubbing: bool,
    /// Index into `STEP_PRESETS` for the `<`/`>` step buttons.
    step_index: usize,
}

/// `<`/`>` step size, as a fraction of `duration_secs` — linear in real
/// replay time since `cursor_micros` maps progress to timestamps linearly.
/// Cycled by clicking the step-size button, wrapping back to the start.
const STEP_PRESETS: [f32; 7] = [0.0001, 0.001, 0.01, 0.02, 0.05, 0.10, 0.25];
const DEFAULT_STEP_INDEX: usize = 2;

const STATUS_X: i32 = 10;
const STATUS_WIDTH: i32 = 700;

fn back_rect() -> Rectangle {
    Rectangle::new(18.0, 50.0, 40.0, 30.0)
}

fn step_size_rect() -> Rectangle {
    Rectangle::new(64.0, 50.0, 66.0, 30.0)
}

fn forward_rect() -> Rectangle {
    Rectangle::new(136.0, 50.0, 40.0, 30.0)
}

fn slower_rect() -> Rectangle {
    Rectangle::new(184.0, 50.0, 78.0, 30.0)
}

fn pause_rect() -> Rectangle {
    Rectangle::new(270.0, 50.0, 78.0, 30.0)
}

fn faster_rect() -> Rectangle {
    Rectangle::new(356.0, 50.0, 78.0, 30.0)
}

fn restart_rect() -> Rectangle {
    Rectangle::new(442.0, 50.0, 68.0, 30.0)
}

fn close_rect() -> Rectangle {
    Rectangle::new(676.0, 15.0, 28.0, 28.0)
}

/// Hit area for the progress slider: taller than the thin line it draws
/// (`draw_overlay_with_status`) so it is easy to grab, but capped above the
/// `y = 50` button row so it never steals their clicks.
fn progress_track_rect() -> Rectangle {
    Rectangle::new((STATUS_X + 8) as f32, 30.0, (STATUS_WIDTH - 16) as f32, 20.0)
}

impl ReplayClock {
    pub fn new(
        timestamps_micros: impl IntoIterator<Item = i64>,
        duration_secs: f32,
        fallback_now_micros: i64,
    ) -> Self {
        let mut timestamps = timestamps_micros.into_iter();
        let first = timestamps.next().unwrap_or(fallback_now_micros);
        let (mut start_micros, mut end_micros) = (first, first);
        for timestamp in timestamps {
            start_micros = start_micros.min(timestamp);
            end_micros = end_micros.max(timestamp);
        }
        Self {
            start_micros,
            end_micros,
            duration_secs: duration_secs.max(MIN_DURATION_SECS),
            elapsed_secs: 0.0,
            speed: 1.0,
            paused: false,
            scrubbing: false,
            step_index: DEFAULT_STEP_INDEX,
        }
    }

    pub fn tick(&mut self, frame_seconds: f32) {
        if !self.paused {
            self.elapsed_secs =
                (self.elapsed_secs + frame_seconds.max(0.0) * self.speed).min(self.duration_secs);
        }
    }

    pub fn restart(&mut self) {
        self.elapsed_secs = 0.0;
        self.paused = false;
    }

    pub fn toggle_pause(&mut self) {
        self.paused = !self.paused;
    }

    /// Jump straight to an arbitrary point, as dragging the progress slider
    /// does. Pauses like scrubbing a video player, rather than fighting the
    /// next `tick` for the same frame's position.
    pub fn seek(&mut self, progress: f32) {
        self.elapsed_secs = progress.clamp(0.0, 1.0) * self.duration_secs;
        self.paused = true;
    }

    /// `>`: nudge forward by the current step size.
    pub fn step_forward(&mut self) {
        self.seek(self.progress() + STEP_PRESETS[self.step_index]);
    }

    /// `<`: nudge backward by the current step size.
    pub fn step_backward(&mut self) {
        self.seek(self.progress() - STEP_PRESETS[self.step_index]);
    }

    /// Cycles how big a `<`/`>` press moves, wrapping back to the start.
    pub fn cycle_step_size(&mut self) {
        self.step_index = (self.step_index + 1) % STEP_PRESETS.len();
    }

    pub fn step_fraction(&self) -> f32 {
        STEP_PRESETS[self.step_index]
    }

    pub fn faster(&mut self) {
        self.speed = (self.speed * 2.0).min(MAX_SPEED);
    }

    pub fn slower(&mut self) {
        self.speed = (self.speed * 0.5).max(MIN_SPEED);
    }

    pub fn progress(&self) -> f32 {
        (self.elapsed_secs / self.duration_secs).clamp(0.0, 1.0)
    }

    pub fn is_finished(&self) -> bool {
        self.elapsed_secs >= self.duration_secs
    }

    pub fn is_visible(&self, timestamp_micros: i64) -> bool {
        // Once caught up, become a live viewer: future subscription updates
        // should appear immediately instead of remaining beyond a frozen
        // snapshot end timestamp forever.
        if self.is_finished() {
            return true;
        }
        timestamp_micros <= self.cursor_micros()
    }

    pub fn speed(&self) -> f32 {
        self.speed
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    fn cursor_micros(&self) -> i64 {
        let span = self.end_micros.saturating_sub(self.start_micros);
        self.start_micros
            .saturating_add((span as f64 * self.progress() as f64) as i64)
    }
}

/// Keyboard and pointer/touch controls shared by native and web replay.
/// Returns true when the viewer should close and normal gameplay resume.
pub fn handle_input(rl: &mut RaylibHandle, clock: &mut ReplayClock) -> bool {
    if rl.is_key_pressed(KeyboardKey::KEY_ESCAPE) {
        return true;
    }
    if rl.is_key_pressed(KeyboardKey::KEY_SPACE) {
        clock.toggle_pause();
    }
    if rl.is_key_pressed(KeyboardKey::KEY_R) {
        clock.restart();
    }
    if rl.is_key_pressed(KeyboardKey::KEY_EQUAL) || rl.is_key_pressed(KeyboardKey::KEY_KP_ADD) {
        clock.faster();
    }
    if rl.is_key_pressed(KeyboardKey::KEY_MINUS) || rl.is_key_pressed(KeyboardKey::KEY_KP_SUBTRACT)
    {
        clock.slower();
    }
    if rl.is_key_pressed(KeyboardKey::KEY_RIGHT) {
        clock.step_forward();
    }
    if rl.is_key_pressed(KeyboardKey::KEY_LEFT) {
        clock.step_backward();
    }
    if rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT) {
        let mouse = rl.get_mouse_position();
        if close_rect().check_collision_point_rec(mouse) {
            return true;
        }
        if progress_track_rect().check_collision_point_rec(mouse) {
            clock.scrubbing = true;
        } else if back_rect().check_collision_point_rec(mouse) {
            clock.step_backward();
        } else if step_size_rect().check_collision_point_rec(mouse) {
            clock.cycle_step_size();
        } else if forward_rect().check_collision_point_rec(mouse) {
            clock.step_forward();
        } else if slower_rect().check_collision_point_rec(mouse) {
            clock.slower();
        } else if pause_rect().check_collision_point_rec(mouse) {
            clock.toggle_pause();
        } else if faster_rect().check_collision_point_rec(mouse) {
            clock.faster();
        } else if restart_rect().check_collision_point_rec(mouse) {
            clock.restart();
        }
    }
    if clock.scrubbing {
        if rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT) {
            let track = progress_track_rect();
            clock.seek((rl.get_mouse_position().x - track.x) / track.width);
        } else {
            clock.scrubbing = false;
        }
    }
    false
}

fn draw_button(d: &mut impl RaylibDraw, rect: Rectangle, label: &str) {
    d.draw_rectangle_rec(rect, Color::new(35, 35, 43, 245));
    d.draw_rectangle_lines_ex(rect, 1.0, Color::new(105, 105, 118, 255));
    d.draw_text(
        label,
        rect.x as i32 + 10,
        rect.y as i32 + 8,
        14,
        Color::new(235, 235, 240, 255),
    );
}

pub fn draw_overlay(d: &mut impl RaylibDraw, clock: &ReplayClock, visible: usize, total: usize) {
    draw_overlay_with_status(d, clock, format!("{visible}/{total} tiles"));
}

/// Status variant for the recovered commitlog replay.  A mutation count is
/// deliberately shown separately from the current tile count: overwritten
/// and erased tiles are events too, even though they are not visible at this
/// instant.
pub fn draw_history_overlay(
    d: &mut impl RaylibDraw,
    clock: &ReplayClock,
    visible_tiles: usize,
    applied_events: usize,
    total_events: usize,
) {
    draw_overlay_with_status(
        d,
        clock,
        format!("{visible_tiles} tiles  {applied_events}/{total_events} events"),
    );
}

fn draw_overlay_with_status(d: &mut impl RaylibDraw, clock: &ReplayClock, status: String) {
    let progress = clock.progress();
    let state = if clock.is_finished() {
        "LIVE"
    } else if clock.is_paused() {
        "PAUSED"
    } else {
        "REPLAY"
    };
    let label = format!(
        "{state}  {:>3}%  {status}  x{:.3}",
        (progress * 100.0).round() as u8,
        clock.speed()
    );
    // `RaylibDraw` deliberately does not expose the global default-font
    // measurement helper. Keep this status strip at a stable width; the
    // compact 16px label is designed to fit it.
    let x = STATUS_X;
    let width = STATUS_WIDTH;
    d.draw_rectangle(x, 12, width, 34, Color::new(10, 10, 14, 225));
    let track = progress_track_rect();
    let track_y = 39;
    d.draw_rectangle(x + 8, track_y, width - 16, 3, Color::new(54, 54, 62, 255));
    d.draw_rectangle(
        x + 8,
        track_y,
        ((width - 16) as f32 * progress) as i32,
        3,
        Color::new(245, 245, 250, 255),
    );
    // Draggable handle: the thin fill line alone does not read as an
    // interactive slider, so a knob marks the grabbable point.
    d.draw_circle(
        (track.x + track.width * progress) as i32,
        track_y + 1,
        5.0,
        Color::new(245, 245, 250, 255),
    );
    d.draw_text(&label, x + 12, 20, 16, Color::new(235, 235, 240, 255));
    draw_button(d, back_rect(), "<");
    draw_button(
        d,
        step_size_rect(),
        &format!("{:.2}%", clock.step_fraction() * 100.0),
    );
    draw_button(d, forward_rect(), ">");
    draw_button(d, slower_rect(), "Slower");
    draw_button(
        d,
        pause_rect(),
        if clock.is_paused() { "Resume" } else { "Pause" },
    );
    draw_button(d, faster_rect(), "Faster");
    draw_button(d, restart_rect(), "Restart");
    draw_button(d, close_rect(), "X");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_timestamp_range_to_the_requested_duration() {
        let mut replay = ReplayClock::new([100, 200, 300], 10.0, 0);
        assert!(replay.is_visible(100));
        assert!(!replay.is_visible(101));
        replay.tick(5.0);
        assert!(replay.is_visible(200));
        assert!(!replay.is_visible(201));
        replay.tick(5.0);
        assert!(replay.is_finished());
        assert!(replay.is_visible(i64::MAX));
    }

    #[test]
    fn pause_speed_and_restart_are_deterministic() {
        let mut replay = ReplayClock::new([0, 100], 8.0, 0);
        replay.faster();
        replay.tick(2.0);
        assert_eq!(replay.progress(), 0.5);
        replay.toggle_pause();
        replay.tick(20.0);
        assert_eq!(replay.progress(), 0.5);
        replay.restart();
        assert_eq!(replay.progress(), 0.0);
        assert!(!replay.is_paused());
    }

    #[test]
    fn step_forward_and_backward_move_by_the_current_step_size_and_pause() {
        let mut replay = ReplayClock::new([0, 100], 10.0, 0);
        assert_eq!(replay.step_fraction(), 0.05);
        replay.step_forward();
        assert_eq!(replay.progress(), 0.05);
        assert!(replay.is_paused());
        replay.step_backward();
        assert_eq!(replay.progress(), 0.0);
        // Stepping past either edge clamps instead of wrapping or going negative.
        replay.step_backward();
        assert_eq!(replay.progress(), 0.0);
    }

    #[test]
    fn cycle_step_size_wraps_through_every_preset() {
        let mut replay = ReplayClock::new([0, 100], 10.0, 0);
        let first = replay.step_fraction();
        for _ in 0..STEP_PRESETS.len() {
            replay.cycle_step_size();
        }
        assert_eq!(replay.step_fraction(), first);
    }
}
