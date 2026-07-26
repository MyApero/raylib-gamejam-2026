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
    /// In/out points on the same track as the progress slider, as fractions
    /// of the current span. They only *mark* a window; `Cut` is what
    /// collapses the replay to it. Kept separate from `start`/`end_micros`
    /// so the marks can be dragged back and forth before committing.
    trim_start: f32,
    trim_end: f32,
    /// Which trim handle a drag is currently moving, same reason
    /// `scrubbing` exists: keep tracking once the pointer leaves the track.
    dragging_trim: Option<TrimHandle>,
    /// Fractions the last `apply_trim` used, until a caller consumes them.
    ///
    /// Narrowing this clock's timestamp range is enough for a replay driven
    /// by row timestamps, but the recovered-history replay is paced over
    /// event *indices* in a file, so it has to apply the same cut to its own
    /// range. `apply_trim` resets the handles immediately, so the fractions
    /// are stashed here rather than read back off them.
    pending_cut: Option<(f32, f32)>,
    /// Drives the Download button beside `Cut`. Set by the caller each frame;
    /// only the web client records anything, so natively it stays
    /// `Unavailable` and the button never appears.
    download: DownloadState,
    /// Download was clicked, until a caller consumes it.
    download_requested: bool,
    /// Format prompt is open, covering the overlay.
    prompt_open: bool,
    /// Estimated size labels for the two containers, supplied by the caller
    /// (only it knows the capture resolution and bitrates).
    prompt_estimates: (String, String),
    /// Format picked in the prompt, until a caller consumes it.
    chosen_format: Option<AnimationFormat>,
    /// Whether islands follow the leaderboard re-ranks recorded in the
    /// history, or hold the position they ended up at. `None` hides the
    /// toggle entirely — a single-island replay pins its island to the
    /// origin, so it has nothing to move.
    island_motion: Option<bool>,
}

/// Container an animated export is written to. One "Animation" export is
/// offered up front; this is chosen at download time, once the length and
/// estimated size of each are known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationFormat {
    Webm,
    Gif,
}

/// Lifecycle of the overlay's Download button. The button stays put through
/// all three visible states rather than vanishing once clicked, so there is
/// always feedback about what the export is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadState {
    /// Nothing to export (native, or no format chosen yet) — button hidden.
    Unavailable,
    /// Ready to record with the current range and speed.
    Ready,
    /// Recording in progress; the click is ignored until it finishes.
    Working,
    /// File has been handed to the browser.
    Done,
}

/// The two ends of the trim window, like a video editor's in/out points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrimHandle {
    Start,
    End,
}

/// Smallest window the handles can be squeezed to, as a fraction of the
/// span — stops a cut from producing a zero-length replay.
const MIN_TRIM_GAP: f32 = 0.01;

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

fn cut_rect() -> Rectangle {
    Rectangle::new(516.0, 50.0, 58.0, 30.0)
}

/// Sits immediately right of `Cut`, in the same button row and style. Only
/// drawn once a capture is waiting (`set_download_ready`); drawing it in the
/// canvas is safe because recording has already stopped by then, so it can't
/// end up inside the exported frames the way the progress bar would.
fn download_rect() -> Rectangle {
    Rectangle::new(580.0, 50.0, 92.0, 30.0)
}

/// Second row: the button row above is full, and this switches how the whole
/// replay is laid out rather than how it is played, so it reads better apart
/// from the transport controls anyway.
fn island_motion_rect() -> Rectangle {
    Rectangle::new(18.0, 86.0, 140.0, 30.0)
}

/// Format prompt, centred over the map like the game's other modals.
fn prompt_rect() -> Rectangle {
    Rectangle::new(160.0, 250.0, 400.0, 210.0)
}

fn prompt_webm_rect() -> Rectangle {
    Rectangle::new(180.0, 330.0, 360.0, 44.0)
}

fn prompt_gif_rect() -> Rectangle {
    Rectangle::new(180.0, 382.0, 360.0, 44.0)
}

/// Grab area for a trim handle, centred on its position along the track.
/// Wider than the drawn bar so it stays grabbable on a touchscreen, and
/// tested before `progress_track_rect` so it wins the overlapping click.
fn trim_handle_rect(fraction: f32) -> Rectangle {
    let track = progress_track_rect();
    Rectangle::new(
        track.x + track.width * fraction - 7.0,
        track.y,
        14.0,
        track.height,
    )
}

/// Clearance between the right end of the progress track and the close
/// button. Without it the track ran under X, so dragging the playhead (or an
/// in/out handle) to the end landed on top of Close.
const TRACK_CLOSE_GAP: f32 = 18.0;

/// Hit area for the progress slider: taller than the thin line it draws
/// (`draw_overlay_with_status`) so it is easy to grab, but capped above the
/// `y = 50` button row so it never steals their clicks, and stopped short of
/// the close button on the right.
fn progress_track_rect() -> Rectangle {
    let x = (STATUS_X + 8) as f32;
    let right = close_rect().x - TRACK_CLOSE_GAP;
    Rectangle::new(x, 30.0, right - x, 20.0)
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
            trim_start: 0.0,
            trim_end: 1.0,
            dragging_trim: None,
            pending_cut: None,
            download: DownloadState::Unavailable,
            download_requested: false,
            prompt_open: false,
            prompt_estimates: (String::new(), String::new()),
            chosen_format: None,
            island_motion: None,
        }
    }

    /// Offer the islands-move toggle, starting on. Only the world replay
    /// calls this.
    pub fn enable_island_motion(&mut self) {
        self.island_motion.get_or_insert(true);
    }

    /// True when islands should be drawn at their historical slots. Always
    /// true when the toggle is not offered, which is the behaviour every
    /// replay had before it existed.
    pub fn island_motion(&self) -> bool {
        self.island_motion.unwrap_or(true)
    }

    pub fn prompt_open(&self) -> bool {
        self.prompt_open
    }

    /// Size labels shown in the prompt, e.g. `("~4.1 MB", "~980 KB")`. The
    /// caller owns these because only it knows the capture resolution and the
    /// codec assumptions behind them.
    pub fn set_format_estimates(&mut self, webm: String, gif: String) {
        self.prompt_estimates = (webm, gif);
    }

    /// Format picked in the prompt, consumed once.
    pub fn take_format_choice(&mut self) -> Option<AnimationFormat> {
        self.chosen_format.take()
    }

    pub fn set_download_state(&mut self, state: DownloadState) {
        self.download = state;
        if state != DownloadState::Ready {
            self.download_requested = false;
        }
    }

    /// Wall-clock length of the replay at the current pace, in seconds — what
    /// an exported video would actually run for. `duration_secs` is the
    /// playback length at speed 1.0, so the real time scales inversely.
    pub fn real_duration_secs(&self) -> f32 {
        self.duration_secs / self.speed.max(MIN_SPEED)
    }

    /// True once, after the Download button is clicked.
    pub fn take_download_request(&mut self) -> bool {
        std::mem::take(&mut self.download_requested)
    }

    /// Re-time an existing replay without disturbing its range.
    ///
    /// An export reuses the running clock rather than building a fresh one,
    /// so that a `Cut` the player just made still bounds what gets recorded;
    /// only the pace changes.
    pub fn retime(&mut self, duration_secs: f32) {
        self.duration_secs = duration_secs.max(MIN_DURATION_SECS);
        self.restart();
    }

    /// Fractions of the most recent `apply_trim`, consumed once. Callers that
    /// own an index-paced history use this to narrow it by the same amount.
    pub fn take_applied_cut(&mut self) -> Option<(f32, f32)> {
        self.pending_cut.take()
    }

    /// In/out points as fractions of the current span.
    pub fn trim(&self) -> (f32, f32) {
        (self.trim_start, self.trim_end)
    }

    /// Whether `timestamp_micros` falls inside the replay's window — i.e.
    /// Whether it is one of the events this replay actually animates.
    ///
    /// Distinct from `is_visible`, which asks "has this happened yet at the
    /// current playhead" and is therefore true for everything before the
    /// window too (that paint is the starting canvas). Callers count events
    /// with this so the status line reflects a `Cut`; counting every row in
    /// the world instead would make the total look frozen.
    pub fn contains(&self, timestamp_micros: i64) -> bool {
        timestamp_micros >= self.start_micros && timestamp_micros <= self.end_micros
    }

    /// Whether the handles actually narrow the window — i.e. `Cut` would do
    /// something. Tolerance rather than `!= 0.0` so a handle nudged back to
    /// the edge by hand still counts as untrimmed.
    pub fn has_trim(&self) -> bool {
        self.trim_start > 0.0005 || self.trim_end < 0.9995
    }

    /// Collapse the replay span to the trimmed window and reset the handles.
    /// Events outside it are simply no longer part of this clock's range,
    /// which is the point: it cuts a long history down to the interesting
    /// slice so an export doesn't have to cover the dead time.
    pub fn apply_trim(&mut self) {
        if !self.has_trim() {
            return;
        }
        let span = (self.end_micros - self.start_micros) as f64;
        let new_start = self.start_micros + (span * self.trim_start as f64) as i64;
        let new_end = self.start_micros + (span * self.trim_end as f64) as i64;
        self.start_micros = new_start;
        self.end_micros = new_end.max(new_start + 1);
        self.pending_cut = Some((self.trim_start, self.trim_end));
        self.trim_start = 0.0;
        self.trim_end = 1.0;
        self.restart();
    }

    fn set_trim_handle(&mut self, handle: TrimHandle, value: f32) {
        let value = value.clamp(0.0, 1.0);
        match handle {
            // Each handle is clamped against the other so they can't cross.
            // The `.max`/`.min` guard the clamp bounds themselves, which
            // would otherwise be inverted (and panic) at the extremes.
            TrimHandle::Start => {
                self.trim_start = value.min((self.trim_end - MIN_TRIM_GAP).max(0.0));
            }
            TrimHandle::End => {
                self.trim_end = value.max((self.trim_start + MIN_TRIM_GAP).min(1.0));
            }
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
    // The format prompt is modal: while it is up it takes every click, so a
    // stray press can't scrub the timeline or close the replay underneath it.
    if clock.prompt_open {
        if rl.is_key_pressed(KeyboardKey::KEY_ESCAPE) {
            clock.prompt_open = false;
            return false;
        }
        if rl.is_mouse_button_pressed(MouseButton::MOUSE_BUTTON_LEFT) {
            let mouse = rl.get_mouse_position();
            if prompt_webm_rect().check_collision_point_rec(mouse) {
                clock.chosen_format = Some(AnimationFormat::Webm);
                clock.prompt_open = false;
            } else if prompt_gif_rect().check_collision_point_rec(mouse) {
                clock.chosen_format = Some(AnimationFormat::Gif);
                clock.prompt_open = false;
            } else if !prompt_rect().check_collision_point_rec(mouse) {
                clock.prompt_open = false; // click-away dismisses
            }
        }
        return false;
    }
    if rl.is_key_pressed(KeyboardKey::KEY_ESCAPE) {
        return true;
    }
    if rl.is_key_pressed(KeyboardKey::KEY_SPACE) {
        // Once the replay has run out there is nothing to un-pause, so the
        // useful action is to play it again — toggling pause at the end just
        // did nothing visible.
        if clock.is_finished() {
            clock.restart();
        } else {
            clock.toggle_pause();
        }
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
        // Trim handles before the track: they sit on top of it, so testing
        // the track first would start a scrub instead of grabbing a handle.
        if trim_handle_rect(clock.trim_start).check_collision_point_rec(mouse) {
            clock.dragging_trim = Some(TrimHandle::Start);
        } else if trim_handle_rect(clock.trim_end).check_collision_point_rec(mouse) {
            clock.dragging_trim = Some(TrimHandle::End);
        } else if cut_rect().check_collision_point_rec(mouse) {
            clock.apply_trim();
        } else if clock.download == DownloadState::Ready
            && download_rect().check_collision_point_rec(mouse)
        {
            // Opens the format prompt rather than recording immediately: the
            // container is chosen there, now that length and size are known.
            clock.prompt_open = true;
            clock.download_requested = true;
        } else if clock.island_motion.is_some()
            && island_motion_rect().check_collision_point_rec(mouse)
        {
            clock.island_motion = Some(!clock.island_motion())
        } else if progress_track_rect().check_collision_point_rec(mouse) {
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
    if let Some(handle) = clock.dragging_trim {
        if rl.is_mouse_button_down(MouseButton::MOUSE_BUTTON_LEFT) {
            let track = progress_track_rect();
            let fraction = (rl.get_mouse_position().x - track.x) / track.width;
            clock.set_trim_handle(handle, fraction);
        } else {
            clock.dragging_trim = None;
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

/// `seconds` as `m:ss`, for the projected length of an exported video.
fn format_mmss(seconds: f32) -> String {
    let total = seconds.max(0.0).round() as u32;
    format!("{}:{:02}", total / 60, total % 60)
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
    // The `m:ss` after the speed is how long an export would actually run
    // for: the replay covers `duration_secs` of playback, so the real elapsed
    // time scales inversely with speed. Reads directly as "the video I get".
    let label = format!(
        "{state}  {:>3}%  {status}  x{:.3}  {}",
        (progress * 100.0).round() as u8,
        clock.speed(),
        format_mmss(clock.real_duration_secs())
    );
    // `RaylibDraw` deliberately does not expose the global default-font
    // measurement helper. Keep this status strip at a stable width; the
    // compact 16px label is designed to fit it.
    let x = STATUS_X;
    let width = STATUS_WIDTH;
    d.draw_rectangle(x, 12, width, 34, Color::new(10, 10, 14, 225));
    let track = progress_track_rect();
    let track_y = 39;
    d.draw_rectangle(x + 8, track_y, track.width as i32, 3, Color::new(54, 54, 62, 255));
    d.draw_rectangle(
        x + 8,
        track_y,
        (track.width * progress) as i32,
        3,
        Color::new(245, 245, 250, 255),
    );
    // Trim window: the spans outside the in/out points are greyed over the
    // track, so the slice `Cut` would keep reads at a glance.
    let (trim_start, trim_end) = clock.trim();
    let dim = Color::new(10, 10, 14, 190);
    if trim_start > 0.0 {
        d.draw_rectangle(
            x + 8,
            track_y - 4,
            (track.width * trim_start) as i32,
            11,
            dim,
        );
    }
    if trim_end < 1.0 {
        let cut_x = x + 8 + (track.width * trim_end) as i32;
        d.draw_rectangle(cut_x, track_y - 4, (x + 8 + track.width as i32) - cut_x, 11, dim);
    }
    // Draggable handle: the thin fill line alone does not read as an
    // interactive slider, so a knob marks the grabbable point.
    d.draw_circle(
        (track.x + track.width * progress) as i32,
        track_y + 1,
        5.0,
        Color::new(245, 245, 250, 255),
    );
    // In/out bars, drawn after the playhead so they stay visible when it
    // passes underneath. Amber to read as "edit", not "playback".
    for fraction in [trim_start, trim_end] {
        d.draw_rectangle(
            (track.x + track.width * fraction) as i32 - 2,
            track_y - 7,
            4,
            17,
            Color::new(250, 190, 90, 255),
        );
    }
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
    // Highlighted only when the handles actually narrow the window, so the
    // button reads as inert until there is something to cut.
    let cut = cut_rect();
    if clock.has_trim() {
        d.draw_rectangle_rec(cut, Color::new(92, 68, 24, 245));
        d.draw_rectangle_lines_ex(cut, 1.0, Color::new(250, 190, 90, 255));
        d.draw_text(
            "Cut",
            cut.x as i32 + 14,
            cut.y as i32 + 8,
            14,
            Color::new(255, 226, 170, 255),
        );
    } else {
        draw_button(d, cut, "Cut");
    }
    // Right of Cut, same row and style. Only present once a capture is
    // waiting, so it never occupies the row during normal playback.
    if let Some((label, fill)) = match clock.download {
        DownloadState::Unavailable => None,
        DownloadState::Ready => Some(("Download", Color::new(40, 70, 100, 245))),
        // Dimmed while recording, green once written, so the button reports
        // progress instead of disappearing the moment it is clicked.
        DownloadState::Working => Some(("Recording", Color::new(52, 52, 62, 245))),
        DownloadState::Done => Some(("Downloaded", Color::new(40, 70, 48, 245))),
    } {
        let download = download_rect();
        d.draw_rectangle_rec(download, fill);
        d.draw_rectangle_lines_ex(download, 1.0, Color::new(120, 120, 135, 255));
        d.draw_text(
            label,
            download.x as i32 + 8,
            download.y as i32 + 8,
            14,
            Color::new(235, 235, 240, 255),
        );
    }
    crate::ui::draw_close_button(d, close_rect());
    if let Some(moving) = clock.island_motion {
        draw_button(
            d,
            island_motion_rect(),
            if moving {
                "Islands: move"
            } else {
                "Islands: fixed"
            },
        );
    }
    if clock.prompt_open {
        draw_format_prompt(d, clock);
    }
}

/// Container picker shown when Download is pressed. Both options record the
/// same replay — same range after any cut, same speed — so the only real
/// decision is the trade-off between size and fidelity, which is why the
/// length and estimated size are on the buttons themselves.
fn draw_format_prompt(d: &mut impl RaylibDraw, clock: &ReplayClock) {
    d.draw_rectangle(0, 0, 720, 720, Color::new(0, 0, 0, 150));
    let panel = prompt_rect();
    d.draw_rectangle_rec(panel, Color::new(24, 24, 30, 250));
    d.draw_rectangle_lines_ex(panel, 2.0, Color::new(90, 90, 100, 255));
    d.draw_text(
        "Export animation",
        panel.x as i32 + 20,
        panel.y as i32 + 16,
        18,
        Color::RAYWHITE,
    );
    d.draw_text(
        &format!(
            "{} long at x{:.3}",
            format_mmss(clock.real_duration_secs()),
            clock.speed()
        ),
        panel.x as i32 + 20,
        panel.y as i32 + 46,
        14,
        Color::LIGHTGRAY,
    );
    // Sizes are projections from the capture settings, not measurements —
    // nothing has been recorded yet — so they are prefixed with "~".
    for (rect, label, detail, fill) in [
        (
            prompt_webm_rect(),
            "WebM",
            format!("{} - full colour, smaller", clock.prompt_estimates.0),
            Color::new(40, 70, 100, 255),
        ),
        (
            prompt_gif_rect(),
            "GIF",
            format!("{} - plays anywhere", clock.prompt_estimates.1),
            Color::new(64, 56, 102, 255),
        ),
    ] {
        d.draw_rectangle_rec(rect, fill);
        d.draw_rectangle_lines_ex(rect, 1.0, Color::new(120, 120, 135, 255));
        d.draw_text(
            label,
            rect.x as i32 + 14,
            rect.y as i32 + 6,
            16,
            Color::RAYWHITE,
        );
        d.draw_text(
            &detail,
            rect.x as i32 + 14,
            rect.y as i32 + 26,
            12,
            Color::new(200, 200, 210, 255),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_handles_cannot_cross_or_invert() {
        let mut replay = ReplayClock::new([0, 1000], 10.0, 0);
        // Dragging the in point past the out point parks it MIN_TRIM_GAP short.
        replay.set_trim_handle(TrimHandle::Start, 5.0);
        let (start, end) = replay.trim();
        assert!(start < end, "start {start} must stay below end {end}");
        assert!((start - (1.0 - MIN_TRIM_GAP)).abs() < 1e-6, "got {start}");
        // And the same from the other side, including past the lower bound.
        replay.set_trim_handle(TrimHandle::End, -5.0);
        let (start, end) = replay.trim();
        assert!(start < end, "start {start} must stay below end {end}");
    }

    #[test]
    fn cut_narrows_the_span_to_the_trimmed_window() {
        let mut replay = ReplayClock::new([0, 1000], 10.0, 0);
        replay.set_trim_handle(TrimHandle::Start, 0.25);
        replay.set_trim_handle(TrimHandle::End, 0.75);
        assert!(replay.has_trim());
        replay.apply_trim();
        assert_eq!(replay.trim(), (0.0, 1.0), "handles reset after cutting");
        assert!(!replay.has_trim());

        // The window is now [250, 750]: the whole replay animates just those
        // events. Anything earlier is already painted at progress 0 (it's the
        // starting canvas, not something to re-animate), and anything later
        // is still in the future.
        replay.seek(0.0);
        assert!(replay.is_visible(0), "pre-window paint is the starting state");
        assert!(!replay.is_visible(500), "mid-window paint has not happened yet");
        replay.seek(0.5);
        assert!(replay.is_visible(500), "mid-window paint appears halfway");
        assert!(!replay.is_visible(740), "late-window paint still pending");
    }

    #[test]
    fn cut_reduces_the_event_count_callers_display() {
        // What the status line counts: events inside the window. Before the
        // cut every event is in range; after it, the ones outside are not.
        let events = [0i64, 250, 500, 750, 1000];
        let mut replay = ReplayClock::new(events, 10.0, 0);
        assert_eq!(events.iter().filter(|t| replay.contains(**t)).count(), 5);
        replay.set_trim_handle(TrimHandle::Start, 0.25);
        replay.set_trim_handle(TrimHandle::End, 0.75);
        replay.apply_trim();
        assert_eq!(
            events.iter().filter(|t| replay.contains(**t)).count(),
            3,
            "cut should drop the events outside the window"
        );
    }

    #[test]
    fn cut_without_a_trim_is_a_no_op() {
        let mut replay = ReplayClock::new([100, 900], 10.0, 0);
        assert!(!replay.has_trim());
        replay.apply_trim();
        replay.seek(1.0);
        assert!(replay.is_visible(100) && replay.is_visible(900));
    }

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
