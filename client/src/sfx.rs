//! F12: sound effects for the existing toast events (merge, gift claim,
//! level up, rejected action). Bytes are embedded via `include_bytes!`
//! rather than loaded from a file path at runtime, so native and web share
//! one asset pipeline — the web target has no real filesystem, and staging
//! files into emscripten's virtual FS (`--preload-file`) would mean a
//! second, divergent asset story for no benefit at this size (~170 KB
//! total, see `client/assets/sfx/LICENSE.md`).
//!
//! CC0 assets (raylib's own bundled rFXGen sfx) — see
//! `client/assets/sfx/LICENSE.md` for the source mapping.

use raylib::prelude::*;

pub struct Sfx<'aud> {
    pub merge: Sound<'aud>,
    pub gift: Sound<'aud>,
    pub levelup: Sound<'aud>,
    pub error: Sound<'aud>,
}

impl<'aud> Sfx<'aud> {
    /// The embedded bytes are our own known-good assets (checked in, never
    /// user-supplied), so a decode failure here is a build-time asset bug,
    /// not a runtime condition to degrade gracefully from — `expect` is
    /// correct. Audio DEVICE availability is the real environmental
    /// boundary (headless/CI, no sound card, browser autoplay block); that
    /// is handled by the caller treating `RaylibAudio::init_audio_device`
    /// itself as fallible, before this ever runs.
    pub fn load(audio: &'aud RaylibAudio) -> Self {
        let load_one = |bytes: &[u8]| -> Sound<'aud> {
            let wave = audio.new_wave_from_memory(".wav", bytes).expect("embedded sfx is a valid wav");
            audio.new_sound_from_wave(&wave).expect("sound from embedded wave")
        };
        Sfx {
            merge: load_one(include_bytes!("../assets/sfx/merge.wav")),
            gift: load_one(include_bytes!("../assets/sfx/gift.wav")),
            levelup: load_one(include_bytes!("../assets/sfx/levelup.wav")),
            error: load_one(include_bytes!("../assets/sfx/error.wav")),
        }
    }
}
