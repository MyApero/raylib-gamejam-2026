//! F12: sound effects for the toast events (merge, XP gain, level up, new
//! color unlock, rejected action) plus a looping background theme and a
//! grab-bag of meme sounds for gift opening. Bytes are embedded via
//! `include_bytes!` rather than loaded from a file path at runtime, so
//! native and web share one asset pipeline — the web target has no real
//! filesystem, and staging files into emscripten's virtual FS
//! (`--preload-file`) would mean a second, divergent asset story for no
//! benefit at this size (~2.5 MB total, see `client/assets/sfx/LICENSE.md`).
//!
//! `merge.wav` / `error.wav` are CC0 (raylib's own bundled rFXGen sfx) — see
//! `client/assets/sfx/LICENSE.md` for the source mapping. The rest are
//! unlicensed placeholder assets; see the copyright note in the sound-rework
//! plan before shipping publicly.
//!
//! Everything non-`.wav` is OGG, not MP3: raylib's vendored MP3 decoder
//! (dr_mp3) reliably aborts partway through decode on the
//! `wasm32-unknown-emscripten` target (reproduced independently of embedded
//! asset size/heap headroom — WAV and OGG both decode fine there, only MP3
//! traps), so MP3 is web-incompatible in this build regardless of file.
//! Native is unaffected; this is wasm/ASYNCIFY-specific.

use raylib::prelude::*;

pub struct Sfx<'aud> {
    pub merge: Sound<'aud>,
    pub error: Sound<'aud>,
    pub xp: Sound<'aud>,
    pub levelup: Sound<'aud>,
    pub new_color: Sound<'aud>,
    pub gift_memes: [Sound<'aud>; 6],
    pub theme: Music<'aud>,
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
        let load_one = |ext: &str, bytes: &'static [u8]| -> Sound<'aud> {
            let wave = audio.new_wave_from_memory(ext, bytes).expect("embedded sfx is a valid audio file");
            audio.new_sound_from_wave(&wave).expect("sound from embedded wave")
        };

        let mut theme = audio
            .new_music_from_memory(".ogg", include_bytes!("../assets/theme.ogg"))
            .expect("embedded theme is a valid ogg");
        theme.set_looping(true);
        theme.set_volume(0.35);

        Sfx {
            merge: load_one(".wav", include_bytes!("../assets/sfx/merge.wav")),
            error: load_one(".wav", include_bytes!("../assets/sfx/error.wav")),
            xp: load_one(".ogg", include_bytes!("../assets/xp.ogg")),
            levelup: load_one(".ogg", include_bytes!("../assets/levelup.ogg")),
            new_color: load_one(".ogg", include_bytes!("../assets/new-color.ogg")),
            gift_memes: [
                load_one(".ogg", include_bytes!("../assets/gift/amongus.ogg")),
                load_one(".ogg", include_bytes!("../assets/gift/fah.ogg")),
                load_one(".ogg", include_bytes!("../assets/gift/kylian-dictador-big.ogg")),
                load_one(".ogg", include_bytes!("../assets/gift/kylian-dictador-simple.ogg")),
                load_one(".ogg", include_bytes!("../assets/gift/rizz.ogg")),
                load_one(".ogg", include_bytes!("../assets/gift/undertakers.ogg")),
            ],
            theme,
        }
    }

    /// ~1-in-3 meme instead of the reward sound. Caller supplies RNG rolls
    /// (RNG lives on RaylibHandle, not here): `meme_roll` in `0..=2` (0 =>
    /// meme plays), `meme_idx` in `0..=5` picks which one.
    pub fn play_gift_reward(&self, reward: &Sound<'_>, meme_roll: i32, meme_idx: i32) {
        if meme_roll == 0 {
            self.gift_memes[meme_idx as usize].play();
        } else {
            reward.play();
        }
    }
}
