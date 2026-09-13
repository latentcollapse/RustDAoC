//! Audio backend (leg 9) — play the client's own WAVs via `rodio`.
//!
//! Decode path: [`caer_assets::soundmap`] maps logical `sounds.dat` names → files.
//! This module is the playback half. Until the full name table exists, only mapped
//! names play; unknown names are silent (not invented).
//!
//! Product integration (INT) should consume [`crate::audio_bus::AudioBus`], not this
//! device directly. Do not treat an unmapped logical name as a successful event.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use caer_assets::soundmap::{pick_variant, resolve_logical_wavs};
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink};

/// Holds the CPAL output stream. Dropping this stops all audio.
pub struct Audio {
    _stream: OutputStream,
    handle: OutputStreamHandle,
    client_root: PathBuf,
    /// Incremented per play so multi-variant names rotate.
    salt: u64,
}

impl Audio {
    /// Open the default output device. `None` if the host has no audio (CI / SSH).
    #[must_use]
    pub fn try_open(client_root: impl Into<PathBuf>) -> Option<Self> {
        let (stream, handle) = OutputStream::try_default().ok()?;
        Some(Self {
            _stream: stream,
            handle,
            client_root: client_root.into(),
            salt: 0,
        })
    }

    /// Play a logical sound name once. Returns `false` if unmapped, missing file, or decode fail.
    pub fn play_logical(&mut self, logical: &str) -> bool {
        let Some(paths) = resolve_logical_wavs(&self.client_root, logical) else {
            log::debug!("caer-audio: no mapping for {logical}");
            return false;
        };
        self.salt = self.salt.wrapping_add(1);
        let Some(path) = pick_variant(&paths, self.salt) else {
            return false;
        };
        self.play_file(path)
    }

    /// Play a WAV path directly (tests / diagnostics).
    pub fn play_file(&self, path: &Path) -> bool {
        let Ok(file) = File::open(path) else {
            log::warn!("caer-audio: open failed {}", path.display());
            return false;
        };
        let Ok(decoder) = Decoder::new(BufReader::new(file)) else {
            log::warn!("caer-audio: decode failed {}", path.display());
            return false;
        };
        let Ok(sink) = Sink::try_new(&self.handle) else {
            return false;
        };
        sink.append(decoder);
        // Detach: keep playing after this call returns. Sink must outlive the append;
        // `detach` transfers ownership to the rodio mixer thread.
        sink.detach();
        true
    }

    #[must_use]
    pub fn client_root(&self) -> &Path {
        &self.client_root
    }
}

/// Shared handle for the live client (optional — headless / no device → None).
pub type SharedAudio = Arc<std::sync::Mutex<Option<Audio>>>;

/// Default ring capacity when diagnostic retention is enabled (`CAER_AUDIO_SMOKE`).
pub const AUDIO_CALL_LOG_CAP: usize = 256;

/// Records logical names requested for playback (System 8 / `CAER_AUDIO_SMOKE` instrument).
///
/// Assert the *call* with the right name — hearing is not automatable. A missing output device
/// still records; [`play_logged`] never panics when `audio` is `None`.
///
/// Codex M1: production retention is off by default. Ordinary play must not allocate a growing
/// `Vec<String>` for the process lifetime. Diagnostic mode uses a fixed-cap ring.
#[derive(Debug, Clone)]
pub struct AudioCallLog {
    /// Retained recent logical names (empty when retention is off).
    calls: Vec<String>,
    /// Max retained entries; `0` disables string retention.
    cap: usize,
    /// Total logical play requests observed (always incremented).
    total: u64,
}

impl Default for AudioCallLog {
    fn default() -> Self {
        Self::disabled()
    }
}

impl AudioCallLog {
    /// No string retention — product default (Codex M1).
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            calls: Vec::new(),
            cap: 0,
            total: 0,
        }
    }

    /// Bounded ring for tests / `CAER_AUDIO_SMOKE`.
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        let cap = cap.max(1);
        Self {
            calls: Vec::with_capacity(cap),
            cap,
            total: 0,
        }
    }

    /// Compatible with older call sites: diagnostic ring at [`AUDIO_CALL_LOG_CAP`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(AUDIO_CALL_LOG_CAP)
    }

    /// Product constructor: retain only when `CAER_AUDIO_SMOKE` is set (any value).
    #[must_use]
    pub fn from_env() -> Self {
        match std::env::var_os("CAER_AUDIO_SMOKE") {
            Some(v) if !v.is_empty() => Self::with_capacity(AUDIO_CALL_LOG_CAP),
            _ => Self::disabled(),
        }
    }

    pub fn record(&mut self, logical: &str) {
        self.total = self.total.wrapping_add(1);
        if self.cap == 0 {
            return;
        }
        if self.calls.len() >= self.cap {
            self.calls.remove(0);
        }
        self.calls.push(logical.to_string());
    }

    pub fn clear(&mut self) {
        self.calls.clear();
        self.total = 0;
    }

    #[must_use]
    pub fn calls(&self) -> &[String] {
        &self.calls
    }

    #[must_use]
    pub fn retained_len(&self) -> usize {
        self.calls.len()
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.cap
    }

    #[must_use]
    pub fn total(&self) -> u64 {
        self.total
    }

    #[must_use]
    pub fn contains(&self, logical: &str) -> bool {
        self.calls.iter().any(|c| c == logical)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }
}

/// Record `logical` then play through an optional device. `None` device → silent no-op (logged).
pub fn play_logged(audio: &mut Option<Audio>, log: &mut AudioCallLog, logical: &str) -> bool {
    log.record(logical);
    match audio.as_mut() {
        Some(a) => a.play_logical(logical),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_logged_records_without_device_and_does_not_panic() {
        let mut audio: Option<Audio> = None;
        let mut log = AudioCallLog::new();
        assert!(!play_logged(&mut audio, &mut log, "g_Thunder"));
        assert!(log.contains("g_Thunder"));
        assert_eq!(log.calls(), ["g_Thunder"]);
    }

    /// Codex M1: millions of events leave retained telemetry under a fixed bound.
    #[test]
    fn audio_call_log_retention_is_bounded() {
        let mut log = AudioCallLog::with_capacity(8);
        for i in 0..1_000_000u32 {
            log.record(&format!("sfx_{i}"));
        }
        assert_eq!(log.retained_len(), 8);
        assert_eq!(log.total(), 1_000_000);
        assert!(log.contains("sfx_999999"));
        assert!(!log.contains("sfx_0"));
    }

    /// Codex M1: disabled product mode allocates no call strings.
    #[test]
    fn audio_call_log_disabled_retains_nothing() {
        let mut log = AudioCallLog::disabled();
        for i in 0..10_000u32 {
            log.record(&format!("sfx_{i}"));
        }
        assert!(log.is_empty());
        assert_eq!(log.capacity(), 0);
        assert_eq!(log.total(), 10_000);
        assert!(!log.contains("sfx_0"));
    }

    #[test]
    fn thunder_file_decodes_when_client_present() {
        let Some(root) = std::env::var_os("CAER_CLIENT") else {
            eprintln!("skip: CAER_CLIENT unset");
            return;
        };
        let root = PathBuf::from(root);
        let paths = resolve_logical_wavs(&root, "g_Thunder").expect("thunder mapping");
        let file = File::open(&paths[0]).expect("open thunder1");
        Decoder::new(BufReader::new(file)).expect("decode thunder1.wav");
        // Opening an output device is optional — CI may have none. Mapping+decode is the
        // falsifier that "backend can consume the client's own file".
    }
}
