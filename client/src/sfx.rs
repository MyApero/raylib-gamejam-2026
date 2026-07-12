//! F12: sound effects for the toast events (merge, XP gain, level up, new
//! color unlock, rejected action) plus a looping background theme and a
//! grab-bag of meme sounds for gift opening. Bytes are embedded via
//! `include_bytes!` rather than loaded from a file path at runtime, so
//! native and web share one asset pipeline — the web target has no real
//! filesystem, and staging files into emscripten's virtual FS
//! (`--preload-file`) would mean a second, divergent asset story for no
//! benefit at this size (~2.6 MB total).
//!
//! `error.wav` is CC0 (raylib's own bundled rFXGen sfx) — see
//! `client/assets/sfx/LICENSE.md` for the source mapping. `merge`/`xp`/
//! `levelup`/`new_color`/`theme` live in `client/assets/free_rights_sfx/`,
//! rights-cleared replacements for the original placeholder assets. The
//! gift memes remain unlicensed placeholders; see the copyright note in the
//! sound-rework plan before shipping publicly.
//!
//! Everything non-`.wav` is OGG, not MP3: raylib's vendored MP3 decoder
//! (dr_mp3) reliably aborts partway through decode on the
//! `wasm32-unknown-emscripten` target (reproduced independently of embedded
//! asset size/heap headroom — WAV and OGG both decode fine there, only MP3
//! traps), so MP3 is web-incompatible in this build regardless of file —
//! `free_rights_sfx/levelup.mp3` was converted to `.ogg` for this reason.
//! Native is unaffected; this is wasm/ASYNCIFY-specific. `gift_memes` below
//! silently skips any `.mp3` it finds in `assets/gift/` for the same
//! reason — convert to `.ogg` before dropping a new meme in that folder if
//! it needs to play on web.
//!
//! `assets/gift/` is embedded as a whole directory (`include_dir!`, below)
//! rather than one `include_bytes!` per file: it's a grab-bag that changes
//! shape often (add/remove/rename a meme clip), and a fixed list here would
//! need editing — and silently drift out of sync — every time it does.

use include_dir::{include_dir, Dir};
use raylib::prelude::*;

static GIFT_DIR: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/assets/gift");

pub struct Sfx<'aud> {
    pub merge: Sound<'aud>,
    pub error: Sound<'aud>,
    pub xp: Sound<'aud>,
    pub levelup: Sound<'aud>,
    pub new_color: Sound<'aud>,
    pub gift_memes: Vec<Sound<'aud>>,
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
            .new_music_from_memory(".ogg", include_bytes!("../assets/free_rights_sfx/theme.ogg"))
            .expect("embedded theme is a valid ogg");
        theme.set_looping(true);
        theme.set_volume(0.35);

        let gift_memes: Vec<Sound<'aud>> = GIFT_DIR
            .files()
            .filter_map(|f| match f.path().extension()?.to_str()? {
                ext @ ("wav" | "ogg") => Some(load_one(&format!(".{ext}"), f.contents())),
                _ => None, // .mp3 (and anything else) skipped — see module doc comment
            })
            .collect();

        Sfx {
            merge: load_one(".wav", include_bytes!("../assets/free_rights_sfx/merge.wav")),
            error: load_one(".wav", include_bytes!("../assets/sfx/error.wav")),
            xp: load_one(".wav", include_bytes!("../assets/free_rights_sfx/xp.wav")),
            levelup: load_one(".ogg", include_bytes!("../assets/free_rights_sfx/levelup.ogg")),
            new_color: load_one(".wav", include_bytes!("../assets/free_rights_sfx/new-color.wav")),
            gift_memes,
            theme,
        }
    }

    /// ~1-in-3 meme instead of the reward sound. Caller supplies RNG rolls
    /// (RNG lives on RaylibHandle, not here): `meme_roll` in `0..=2` (0 =>
    /// meme plays). `raw_idx` can be any non-negative value — folded down
    /// to a valid `gift_memes` index with `rem_euclid`, so callers don't
    /// need to know how many memes are currently loaded.
    pub fn play_gift_reward(&self, reward: &Sound<'_>, meme_roll: i32, raw_idx: i32) {
        if meme_roll == 0 && !self.gift_memes.is_empty() {
            let idx = raw_idx.rem_euclid(self.gift_memes.len() as i32) as usize;
            self.gift_memes[idx].play();
        } else {
            reward.play();
        }
    }
}
