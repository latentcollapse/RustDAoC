//! Typed product-audio bus (Macro-batch 2 lane AUD / leg 9).
//!
//! One resolution boundary: [`LogicalSoundEvent`] → [`caer_assets::soundmap`] → shipped WAV.
//! Unmapped names are not success. Synthetic beeps are not used.
//!
//! INT wires this into `rustdaoc`; this module does not.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use caer_assets::soundmap::{pick_variant, resolve_logical_wavs};
use caer_assets::zonesounds::{SoundShape, Spacing, ZoneSound, ZoneSounds};
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source, SpatialSink};

use crate::audio::AudioCallLog;

/// Max retained diagnostic bus records (always bounded).
pub const BUS_CALL_LOG_CAP: usize = 256;
/// Max logical→path cache entries (including cached `Unmapped`).
const RESOLVE_CACHE_CAP: usize = 256;
/// Max decoded PCM clips retained for the rodio backend.
const DECODE_CACHE_CAP: usize = 64;
/// Max concurrent one-shot sinks on the rodio backend (bounded voice table).
pub const ONESHOT_SINK_CAP: usize = 32;
/// Max pending intermittent zone one-shots.
const SCHEDULE_CAP: usize = 128;
/// Max distinct missing/unmapped names that emit a warn (then stay silent).
const MISSING_REPORT_CAP: usize = 256;

/// Mixer category. Master is applied on top of the matching bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SoundCategory {
    Ui,
    Movement,
    Combat,
    Spell,
    DeathRevive,
    Ambient,
    Music,
}

/// Optional world-space playback parameters from `sounds.dat` (radius is client units).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpatialParams {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub radius: u32,
}

/// One logical play request. The name is resolved through soundmap; this type does not invent files.
#[derive(Debug, Clone, PartialEq)]
pub struct LogicalSoundEvent {
    pub category: SoundCategory,
    pub logical: String,
    pub looping: bool,
    pub spatial: Option<SpatialParams>,
    /// Authored volume in `0.0..=1.0` (from `sounds.dat` volume/100 when present).
    pub volume: f32,
}

impl LogicalSoundEvent {
    #[must_use]
    pub fn oneshot(category: SoundCategory, logical: impl Into<String>) -> Self {
        Self {
            category,
            logical: logical.into(),
            looping: false,
            spatial: None,
            volume: 1.0,
        }
    }

    #[must_use]
    pub fn looping(category: SoundCategory, logical: impl Into<String>) -> Self {
        Self {
            category,
            logical: logical.into(),
            looping: true,
            spatial: None,
            volume: 1.0,
        }
    }
}

/// Master / music / effects / ambient sliders plus mute and a hard disable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioSettings {
    pub master: f32,
    pub music: f32,
    pub effects: f32,
    pub ambient: f32,
    pub muted: bool,
    /// Hard off: no backend playback; game state continues.
    pub disabled: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            master: 1.0,
            music: 1.0,
            effects: 1.0,
            ambient: 1.0,
            muted: false,
            disabled: false,
        }
    }
}

impl AudioSettings {
    #[must_use]
    pub fn silence() -> Self {
        Self {
            muted: true,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn gain_for(self, category: SoundCategory) -> f32 {
        if self.muted || self.disabled {
            return 0.0;
        }
        let bus = match category {
            SoundCategory::Music => self.music,
            SoundCategory::Ambient => self.ambient,
            SoundCategory::Ui
            | SoundCategory::Movement
            | SoundCategory::Combat
            | SoundCategory::Spell
            | SoundCategory::DeathRevive => self.effects,
        };
        (self.master * bus).clamp(0.0, 1.0)
    }

    #[must_use]
    pub fn playback_allowed(self) -> bool {
        !self.muted && !self.disabled && self.master > 0.0
    }
}

/// Result of [`AudioBus::play`]. Only [`PlayOutcome::Played`] is a successful sound event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayOutcome {
    Played,
    Unmapped,
    MissingAsset,
    Muted,
    Disabled,
    Deduped,
}

impl PlayOutcome {
    #[must_use]
    pub fn is_success(self) -> bool {
        matches!(self, Self::Played)
    }
}

/// Which output path the bus is using.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDeviceKind {
    Null,
    Rodio,
}

/// One retained diagnostic record (null-device / CI surface).
#[derive(Debug, Clone, PartialEq)]
pub struct BusCall {
    pub logical: String,
    pub category: SoundCategory,
    pub outcome: PlayOutcome,
    pub spatial: Option<SpatialParams>,
    pub looping: bool,
}

/// Looping voice currently owned by the bus.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveLoop {
    pub logical: String,
    pub category: SoundCategory,
    pub spatial: Option<SpatialParams>,
}

/// Null device: records backend plays without CPAL / hardware.
#[derive(Debug, Default)]
pub struct NullAudioDevice {
    backend_plays: u64,
    loops: Vec<ActiveLoop>,
    oneshots: Vec<ActiveLoop>,
}

impl NullAudioDevice {
    #[must_use]
    pub fn backend_plays(&self) -> u64 {
        self.backend_plays
    }

    #[must_use]
    pub fn loops(&self) -> &[ActiveLoop] {
        &self.loops
    }
}

#[derive(Clone)]
struct DecodedClip {
    channels: u16,
    sample_rate: u32,
    samples: Arc<[f32]>,
}

struct Lru<V> {
    map: HashMap<String, V>,
    order: VecDeque<String>,
    cap: usize,
}

impl<V> Lru<V> {
    fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    fn get(&mut self, key: &str) -> Option<&V> {
        if !self.map.contains_key(key) {
            return None;
        }
        self.touch(key);
        self.map.get(key)
    }

    fn insert(&mut self, key: String, value: V) {
        if self.map.contains_key(&key) {
            self.order.retain(|k| k != &key);
        } else {
            while self.map.len() >= self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                } else {
                    break;
                }
            }
        }
        self.order.push_back(key.clone());
        self.map.insert(key, value);
    }

    fn touch(&mut self, key: &str) {
        self.order.retain(|k| k != key);
        self.order.push_back(key.to_string());
    }

    fn len(&self) -> usize {
        self.map.len()
    }
}

struct ScheduledOneshot {
    due_secs: f32,
    event: LogicalSoundEvent,
}

enum PlaybackSink {
    Mono(Sink),
    Spatial(SpatialSink),
}

impl PlaybackSink {
    fn stop(&self) {
        match self {
            Self::Mono(s) => s.stop(),
            Self::Spatial(s) => s.stop(),
        }
    }

    fn empty(&self) -> bool {
        match self {
            Self::Mono(s) => s.empty(),
            Self::Spatial(s) => s.empty(),
        }
    }

    fn set_emitter(&self, pos: [f32; 3]) {
        if let Self::Spatial(s) = self {
            s.set_emitter_position(pos);
        }
    }
}

struct ActiveVoice {
    category: SoundCategory,
    logical: String,
    spatial: Option<SpatialParams>,
    sink: PlaybackSink,
}

struct RodioBackend {
    _stream: OutputStream,
    handle: OutputStreamHandle,
    loop_sinks: HashMap<(SoundCategory, String), ActiveVoice>,
    oneshots: Vec<ActiveVoice>,
}

/// Iterator source over cached Arc PCM — no per-play full-buffer copy.
struct ArcClipSource {
    samples: Arc<[f32]>,
    channels: u16,
    sample_rate: u32,
    i: usize,
}

impl Iterator for ArcClipSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let s = *self.samples.get(self.i)?;
        self.i += 1;
        Some(s)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.samples.len().saturating_sub(self.i);
        (n, Some(n))
    }
}

impl Source for ArcClipSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<std::time::Duration> {
        let denom = f64::from(self.sample_rate) * f64::from(self.channels.max(1));
        Some(std::time::Duration::from_secs_f64(
            self.samples.len() as f64 / denom,
        ))
    }
}

fn emitter_relative(listener: [f32; 3], spatial: SpatialParams) -> [f32; 3] {
    const SCALE: f32 = 1.0 / 200.0;
    [
        (spatial.x - listener[0]) * SCALE,
        (spatial.y - listener[1]) * SCALE,
        (spatial.z - listener[2]) * SCALE,
    ]
}

enum Backend {
    Null(NullAudioDevice),
    Rodio(RodioBackend),
}

/// Product audio integration surface for INT.
pub struct AudioBus {
    client_root: PathBuf,
    settings: AudioSettings,
    backend: Backend,
    salt: u64,
    resolve_cache: Lru<Option<Vec<PathBuf>>>,
    decode_cache: Lru<DecodedClip>,
    reported: HashSet<String>,
    records: VecDeque<BusCall>,
    record_cap: usize,
    call_log: AudioCallLog,
    scheduled: VecDeque<ScheduledOneshot>,
    active_zone: Option<u16>,
    directory_resolves: u64,
    resolve_hits: u64,
    decode_loads: u64,
    decode_hits: u64,
    listener: [f32; 3],
}

impl AudioBus {
    /// CI / headless constructor: never opens an output device.
    #[must_use]
    pub fn null(client_root: impl Into<PathBuf>) -> Self {
        Self::new(
            client_root.into(),
            Backend::Null(NullAudioDevice::default()),
            BUS_CALL_LOG_CAP,
        )
    }

    /// Open the host output device; fall back to [`Self::null`] when none exists.
    #[must_use]
    pub fn try_open(client_root: impl Into<PathBuf>) -> Self {
        let root = client_root.into();
        match OutputStream::try_default() {
            Ok((stream, handle)) => Self::new(
                root,
                Backend::Rodio(RodioBackend {
                    _stream: stream,
                    handle,
                    loop_sinks: HashMap::new(),
                    oneshots: Vec::new(),
                }),
                if std::env::var_os("CAER_AUDIO_SMOKE").is_some_and(|v| !v.is_empty()) {
                    BUS_CALL_LOG_CAP
                } else {
                    0
                },
            ),
            Err(_) => Self::null(root),
        }
    }

    fn new(client_root: PathBuf, backend: Backend, record_cap: usize) -> Self {
        Self {
            client_root,
            settings: AudioSettings::default(),
            backend,
            salt: 0,
            resolve_cache: Lru::new(RESOLVE_CACHE_CAP),
            decode_cache: Lru::new(DECODE_CACHE_CAP),
            reported: HashSet::new(),
            records: VecDeque::new(),
            record_cap,
            call_log: if record_cap == 0 {
                AudioCallLog::disabled()
            } else {
                AudioCallLog::with_capacity(record_cap)
            },
            scheduled: VecDeque::new(),
            active_zone: None,
            directory_resolves: 0,
            resolve_hits: 0,
            decode_loads: 0,
            decode_hits: 0,
            listener: [0.0, 0.0, 0.0],
        }
    }

    #[must_use]
    pub fn client_root(&self) -> &Path {
        &self.client_root
    }

    #[must_use]
    pub fn device_kind(&self) -> AudioDeviceKind {
        match self.backend {
            Backend::Null(_) => AudioDeviceKind::Null,
            Backend::Rodio(_) => AudioDeviceKind::Rodio,
        }
    }

    #[must_use]
    pub fn settings(&self) -> AudioSettings {
        self.settings
    }

    #[must_use]
    pub fn call_log(&self) -> &AudioCallLog {
        &self.call_log
    }

    #[must_use]
    pub fn records(&self) -> Vec<BusCall> {
        self.records.iter().cloned().collect()
    }

    pub fn set_listener(&mut self, x: f32, y: f32, z: f32) {
        let listener = [x, y, z];
        self.listener = listener;
        if let Backend::Rodio(r) = &mut self.backend {
            for voice in r.loop_sinks.values().chain(r.oneshots.iter()) {
                if let Some(sp) = voice.spatial {
                    voice.sink.set_emitter(emitter_relative(listener, sp));
                }
            }
        }
    }

    #[must_use]
    pub fn listener(&self) -> [f32; 3] {
        self.listener
    }

    #[must_use]
    pub fn active_oneshot_count(&self) -> usize {
        match &self.backend {
            Backend::Null(n) => n.oneshots.len(),
            Backend::Rodio(r) => r.oneshots.len(),
        }
    }

    /// Loops + oneshots currently owned (bounded by [`ONESHOT_SINK_CAP`] on the oneshot side).
    #[must_use]
    pub fn voice_table_len(&self) -> usize {
        self.active_loop_count() + self.active_oneshot_count()
    }

    #[must_use]
    pub fn oneshot_count_in(&self, category: SoundCategory) -> usize {
        match &self.backend {
            Backend::Null(n) => n.oneshots.iter().filter(|v| v.category == category).count(),
            Backend::Rodio(r) => r.oneshots.iter().filter(|v| v.category == category).count(),
        }
    }

    #[must_use]
    pub fn loop_count_in(&self, category: SoundCategory) -> usize {
        match &self.backend {
            Backend::Null(n) => n.loops.iter().filter(|v| v.category == category).count(),
            Backend::Rodio(r) => r
                .loop_sinks
                .keys()
                .filter(|(cat, _)| *cat == category)
                .count(),
        }
    }

    pub fn backend_play_count(&self) -> u64 {
        match &self.backend {
            Backend::Null(n) => n.backend_plays,
            Backend::Rodio(r) => r.loop_sinks.len() as u64 + r.oneshots.len() as u64,
        }
    }

    #[must_use]
    pub fn null_backend_plays(&self) -> u64 {
        match &self.backend {
            Backend::Null(n) => n.backend_plays,
            Backend::Rodio(_) => 0,
        }
    }

    #[must_use]
    pub fn active_loops(&self) -> Vec<ActiveLoop> {
        match &self.backend {
            Backend::Null(n) => n.loops.clone(),
            Backend::Rodio(r) => r
                .loop_sinks
                .values()
                .map(|v| ActiveLoop {
                    logical: v.logical.clone(),
                    category: v.category,
                    spatial: v.spatial,
                })
                .collect(),
        }
    }

    #[must_use]
    pub fn active_loop_count(&self) -> usize {
        match &self.backend {
            Backend::Null(n) => n.loops.len(),
            Backend::Rodio(r) => r.loop_sinks.len(),
        }
    }

    #[must_use]
    pub fn scheduled_len(&self) -> usize {
        self.scheduled.len()
    }

    #[must_use]
    pub fn resolve_cache_len(&self) -> usize {
        self.resolve_cache.len()
    }

    #[must_use]
    pub fn decode_cache_len(&self) -> usize {
        self.decode_cache.len()
    }

    #[must_use]
    pub fn directory_resolves(&self) -> u64 {
        self.directory_resolves
    }

    #[must_use]
    pub fn resolve_hits(&self) -> u64 {
        self.resolve_hits
    }

    #[must_use]
    pub fn decode_loads(&self) -> u64 {
        self.decode_loads
    }

    #[must_use]
    pub fn active_zone(&self) -> Option<u16> {
        self.active_zone
    }

    /// Apply mixer settings. Mute/disable/zero-gain stops backend voices immediately.
    pub fn set_settings(&mut self, settings: AudioSettings) {
        self.settings = settings;
        if !settings.playback_allowed() {
            self.stop_all_backend();
            return;
        }
        if settings.gain_for(SoundCategory::Music) <= 0.0 {
            self.stop_category(SoundCategory::Music);
        }
        if settings.gain_for(SoundCategory::Ambient) <= 0.0 {
            self.stop_category(SoundCategory::Ambient);
        }
        if settings.gain_for(SoundCategory::Ui) <= 0.0 {
            self.stop_category(SoundCategory::Ui);
            self.stop_category(SoundCategory::Movement);
            self.stop_category(SoundCategory::Combat);
            self.stop_category(SoundCategory::Spell);
            self.stop_category(SoundCategory::DeathRevive);
        }
    }

    /// Region change: drop old ambient/music ownership, optionally load the new zone table.
    pub fn on_region_changed(
        &mut self,
        zone_id: u16,
        sounds: Option<&ZoneSounds>,
        time_hhmm: u32,
        weather: u32,
        listener_xy: Option<(u32, u32)>,
    ) {
        self.stop_category(SoundCategory::Ambient);
        self.stop_category(SoundCategory::Music);
        self.stop_category(SoundCategory::Combat);
        self.stop_category(SoundCategory::Spell);
        self.stop_category(SoundCategory::DeathRevive);
        self.scheduled.clear();
        self.active_zone = Some(zone_id);
        if let Some(zs) = sounds {
            self.apply_zone_sounds(zone_id, zs, time_hhmm, weather, listener_xy);
        }
    }

    /// Logout / reconnect: drop zone beds **and** in-world combat/spell/death voices.
    pub fn on_logout(&mut self) {
        self.stop_category(SoundCategory::Ambient);
        self.stop_category(SoundCategory::Music);
        self.stop_category(SoundCategory::Combat);
        self.stop_category(SoundCategory::Spell);
        self.stop_category(SoundCategory::DeathRevive);
        self.scheduled.clear();
        self.active_zone = None;
    }

    pub fn on_reconnect(&mut self) {
        self.on_logout();
    }

    /// Entity removal does not own zone beds; hook exists so INT can call it symmetrically.
    pub fn on_object_removed(&mut self, _object_id: u16) {}

    /// Consume `sounds.dat` placement/scheduling. Replaces ambient/music for `zone_id`.
    pub fn apply_zone_sounds(
        &mut self,
        zone_id: u16,
        sounds: &ZoneSounds,
        time_hhmm: u32,
        weather: u32,
        listener_xy: Option<(u32, u32)>,
    ) {
        if self.active_zone != Some(zone_id) {
            self.stop_category(SoundCategory::Ambient);
            self.stop_category(SoundCategory::Music);
            self.scheduled.clear();
        }
        self.active_zone = Some(zone_id);
        for region in &sounds.regions {
            for sound in &region.sounds {
                if !time_in_window(sound.start_time, sound.end_time, time_hhmm) {
                    continue;
                }
                if sound.weather != 0 && sound.weather != weather {
                    continue;
                }
                if let Some((x, y)) = listener_xy {
                    if !shape_contains(&region.shapes, x, y) {
                        continue;
                    }
                }
                let event = event_from_zone_sound(sound, &region.shapes);
                match sound.spacing {
                    Spacing::Fixed(0) => {
                        let _ = self.play(event);
                    }
                    Spacing::Fixed(secs) => {
                        self.push_scheduled(secs as f32, event);
                    }
                    Spacing::Range(lo, hi) => {
                        let span = hi.saturating_sub(lo).max(1);
                        self.salt = self.salt.wrapping_add(1);
                        let secs = lo + (self.salt as u32 % span);
                        self.push_scheduled(secs as f32, event);
                    }
                }
            }
        }
    }

    /// Advance intermittent zone one-shots. Looping beds are owned until lifecycle stop.
    pub fn tick(&mut self, dt_secs: f32) {
        let mut due = Vec::new();
        for item in self.scheduled.iter_mut() {
            item.due_secs -= dt_secs;
        }
        self.scheduled.retain(|item| {
            if item.due_secs <= 0.0 {
                due.push(item.event.clone());
                false
            } else {
                true
            }
        });
        for event in due {
            let _ = self.play(event);
        }
        self.prune_oneshot_sinks();
    }

    /// Resolve + (maybe) play. Unmapped / mute / disable are not [`PlayOutcome::Played`].
    pub fn play(&mut self, event: LogicalSoundEvent) -> PlayOutcome {
        let outcome = self.play_inner(&event);
        self.record(&event, outcome);
        outcome
    }

    fn play_inner(&mut self, event: &LogicalSoundEvent) -> PlayOutcome {
        if self.settings.disabled {
            return PlayOutcome::Disabled;
        }
        let paths = match self.resolve_cached(&event.logical) {
            Some(p) if !p.is_empty() => p,
            _ => {
                self.report_once(
                    &event.logical,
                    "unmapped logical name (soundmap returned None; not a successful event)",
                );
                return PlayOutcome::Unmapped;
            }
        };
        if event.looping && self.has_loop(event.category, &event.logical) {
            return PlayOutcome::Deduped;
        }
        if !self.settings.playback_allowed() || self.settings.gain_for(event.category) <= 0.0 {
            return PlayOutcome::Muted;
        }
        self.salt = self.salt.wrapping_add(1);
        let Some(path) = pick_variant(&paths, self.salt) else {
            return PlayOutcome::Unmapped;
        };
        if !path.is_file() {
            self.report_once(
                &event.logical,
                &format!("mapped path missing {}", path.display()),
            );
            return PlayOutcome::MissingAsset;
        }
        self.start_voice(event, path)
    }

    fn start_voice(&mut self, event: &LogicalSoundEvent, path: &Path) -> PlayOutcome {
        let volume = (event.volume * self.settings.gain_for(event.category)).clamp(0.0, 1.0);
        if matches!(self.backend, Backend::Null(_)) {
            if let Backend::Null(dev) = &mut self.backend {
                dev.backend_plays = dev.backend_plays.saturating_add(1);
                let rec = ActiveLoop {
                    logical: event.logical.clone(),
                    category: event.category,
                    spatial: event.spatial,
                };
                if event.looping {
                    dev.loops.push(rec);
                } else {
                    dev.oneshots.push(rec);
                    while dev.oneshots.len() > ONESHOT_SINK_CAP {
                        dev.oneshots.remove(0);
                    }
                }
            }
            return PlayOutcome::Played;
        }
        self.start_rodio(event, path, volume)
    }

    fn start_rodio(&mut self, event: &LogicalSoundEvent, path: &Path, volume: f32) -> PlayOutcome {
        let key = path.to_string_lossy().into_owned();
        let clip = if let Some(hit) = self.decode_cache.get(&key).cloned() {
            self.decode_hits = self.decode_hits.saturating_add(1);
            hit
        } else {
            match decode_wav(path) {
                Some(clip) => {
                    self.decode_loads = self.decode_loads.saturating_add(1);
                    self.decode_cache.insert(key, clip.clone());
                    clip
                }
                None => {
                    self.report_once(&event.logical, &format!("decode failed {}", path.display()));
                    return PlayOutcome::MissingAsset;
                }
            }
        };
        let source = ArcClipSource {
            samples: Arc::clone(&clip.samples),
            channels: clip.channels,
            sample_rate: clip.sample_rate,
            i: 0,
        };
        let Backend::Rodio(rodio) = &mut self.backend else {
            return PlayOutcome::Disabled;
        };
        let sink = if let Some(sp) = event.spatial {
            let emitter = emitter_relative(self.listener, sp);
            let Ok(s) =
                SpatialSink::try_new(&rodio.handle, emitter, [-0.1, 0.0, 0.0], [0.1, 0.0, 0.0])
            else {
                return PlayOutcome::MissingAsset;
            };
            s.set_volume(volume);
            if event.looping {
                s.append(source.repeat_infinite());
            } else {
                s.append(source);
            }
            PlaybackSink::Spatial(s)
        } else {
            let Ok(s) = Sink::try_new(&rodio.handle) else {
                return PlayOutcome::MissingAsset;
            };
            s.set_volume(volume);
            if event.looping {
                s.append(source.repeat_infinite());
            } else {
                s.append(source);
            }
            PlaybackSink::Mono(s)
        };
        let voice = ActiveVoice {
            category: event.category,
            logical: event.logical.clone(),
            spatial: event.spatial,
            sink,
        };
        if event.looping {
            rodio
                .loop_sinks
                .insert((event.category, event.logical.clone()), voice);
        } else {
            rodio.oneshots.push(voice);
            while rodio.oneshots.len() > ONESHOT_SINK_CAP {
                let old = rodio.oneshots.remove(0);
                old.sink.stop();
            }
        }
        PlayOutcome::Played
    }

    fn resolve_cached(&mut self, logical: &str) -> Option<Vec<PathBuf>> {
        if let Some(hit) = self.resolve_cache.get(logical) {
            self.resolve_hits = self.resolve_hits.saturating_add(1);
            return hit.clone();
        }
        self.directory_resolves = self.directory_resolves.saturating_add(1);
        let resolved = resolve_logical_wavs(&self.client_root, logical);
        self.resolve_cache
            .insert(logical.to_string(), resolved.clone());
        resolved
    }

    fn has_loop(&self, category: SoundCategory, logical: &str) -> bool {
        match &self.backend {
            Backend::Null(n) => n
                .loops
                .iter()
                .any(|l| l.category == category && l.logical == logical),
            Backend::Rodio(r) => r.loop_sinks.contains_key(&(category, logical.to_string())),
        }
    }

    /// Stop one mixer category. Other categories keep their voices.
    pub fn stop_category(&mut self, category: SoundCategory) {
        match &mut self.backend {
            Backend::Null(n) => {
                n.loops.retain(|l| l.category != category);
                n.oneshots.retain(|l| l.category != category);
            }
            Backend::Rodio(r) => {
                r.loop_sinks.retain(|(cat, _), voice| {
                    if *cat == category {
                        voice.sink.stop();
                        false
                    } else {
                        true
                    }
                });
                r.oneshots.retain(|voice| {
                    if voice.category == category {
                        voice.sink.stop();
                        false
                    } else {
                        true
                    }
                });
            }
        }
    }

    fn stop_all_backend(&mut self) {
        match &mut self.backend {
            Backend::Null(n) => {
                n.loops.clear();
                n.oneshots.clear();
            }
            Backend::Rodio(r) => {
                for (_, voice) in r.loop_sinks.drain() {
                    voice.sink.stop();
                }
                for voice in r.oneshots.drain(..) {
                    voice.sink.stop();
                }
            }
        }
        self.scheduled.clear();
    }

    fn prune_oneshot_sinks(&mut self) {
        match &mut self.backend {
            Backend::Rodio(r) => r.oneshots.retain(|v| !v.sink.empty()),
            Backend::Null(_) => {}
        }
    }

    fn push_scheduled(&mut self, due_secs: f32, event: LogicalSoundEvent) {
        if self.scheduled.len() >= SCHEDULE_CAP {
            self.scheduled.pop_front();
        }
        self.scheduled
            .push_back(ScheduledOneshot { due_secs, event });
    }

    fn record(&mut self, event: &LogicalSoundEvent, outcome: PlayOutcome) {
        self.call_log.record(&event.logical);
        if self.record_cap == 0 {
            return;
        }
        if self.records.len() >= self.record_cap {
            self.records.pop_front();
        }
        self.records.push_back(BusCall {
            logical: event.logical.clone(),
            category: event.category,
            outcome,
            spatial: event.spatial,
            looping: event.looping,
        });
    }

    fn report_once(&mut self, logical: &str, why: &str) {
        if self.reported.len() >= MISSING_REPORT_CAP {
            return;
        }
        if self.reported.insert(logical.to_string()) {
            log::warn!(
                "caer-audio: {why}; logical=`{logical}` root={} (reported once)",
                self.client_root.display()
            );
        }
    }
}

fn decode_wav(path: &Path) -> Option<DecodedClip> {
    let file = File::open(path).ok()?;
    let decoder = Decoder::new(BufReader::new(file)).ok()?;
    let channels = decoder.channels();
    let sample_rate = decoder.sample_rate();
    if channels == 0 || sample_rate == 0 {
        return None;
    }
    let samples: Vec<f32> = decoder.convert_samples::<f32>().collect();
    if samples.is_empty() {
        return None;
    }
    Some(DecodedClip {
        channels,
        sample_rate,
        samples: samples.into(),
    })
}

fn classify_zone_logical(name: &str) -> SoundCategory {
    let n = name.to_ascii_lowercase();
    if n.contains("music") || n.starts_with("mp3_") {
        SoundCategory::Music
    } else {
        SoundCategory::Ambient
    }
}

fn event_from_zone_sound(sound: &ZoneSound, shapes: &[SoundShape]) -> LogicalSoundEvent {
    let (x, y) = shapes
        .first()
        .map(|s| {
            (
                (s.x0 as f32 + s.x1 as f32) * 0.5,
                (s.y0 as f32 + s.y1 as f32) * 0.5,
            )
        })
        .unwrap_or((0.0, 0.0));
    LogicalSoundEvent {
        category: classify_zone_logical(&sound.name),
        logical: sound.name.clone(),
        looping: matches!(sound.spacing, Spacing::Fixed(0)),
        spatial: Some(SpatialParams {
            x,
            y,
            z: 0.0,
            radius: sound.radius,
        }),
        volume: (sound.volume as f32 / 100.0).clamp(0.0, 1.0),
    }
}

fn time_in_window(start: u32, end: u32, now: u32) -> bool {
    if start == 0 && end == 0 {
        return true;
    }
    if start <= end {
        now >= start && now <= end
    } else {
        now >= start || now <= end
    }
}

fn shape_contains(shapes: &[SoundShape], x: u32, y: u32) -> bool {
    if shapes.is_empty() {
        return true;
    }
    shapes.iter().any(|s| {
        let (x0, x1) = (s.x0.min(s.x1), s.x0.max(s.x1));
        let (y0, y1) = (s.y0.min(s.y1), s.y0.max(s.y1));
        x >= x0 && x <= x1 && y >= y0 && y <= y1
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use caer_assets::zonesounds::parse;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SCRATCH: AtomicU64 = AtomicU64::new(0);

    fn scratch_client(files: &[&str]) -> PathBuf {
        let n = SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("caer-mb2-aud-{}-{}", std::process::id(), n));
        let sounds = root.join("sounds");
        std::fs::create_dir_all(&sounds).expect("scratch sounds/");
        for f in files {
            std::fs::write(sounds.join(f), b"").expect("touch wav");
        }
        root
    }

    fn cleanup(root: &Path) {
        let _ = std::fs::remove_dir_all(root);
    }

    /// Named falsifier: unmapped logical name is not a successful sound event.
    #[test]
    fn unmapped_logical_name_is_not_success() {
        let root = scratch_client(&["click.wav"]);
        let mut bus = AudioBus::null(&root);
        let out = bus.play(LogicalSoundEvent::oneshot(
            SoundCategory::Ambient,
            "g_PrairieWind",
        ));
        assert_eq!(out, PlayOutcome::Unmapped);
        assert!(!out.is_success());
        assert_eq!(bus.null_backend_plays(), 0);
        assert!(bus
            .records()
            .iter()
            .any(|r| r.logical == "g_PrairieWind" && r.outcome == PlayOutcome::Unmapped));
        cleanup(&root);
    }

    /// Named falsifier: region transition stops/replaces old ambient/music ownership.
    #[test]
    fn region_transition_stops_replaces_old_ambient_music() {
        let root = scratch_client(&[
            "Bed_Test.wav",
            "MusicComponent_CC12-01.wav",
            "Bed_Other.wav",
        ]);
        let mut bus = AudioBus::null(&root);
        let zone_a = parse(
            "[SoundRegion00];[terrain:a]\n\
             Sound00=2, s_Bed_Test, 0, 100, 24, 0, 0, 0\n\
             Sound01=2, s_MusicComponent_CC12-01, 0, 100, 127, 0, 0, 0\n\
             Shape00=0, 0, 0, 65535, 65535\n",
        );
        let zone_b = parse(
            "[SoundRegion00];[terrain:b]\n\
             Sound00=2, s_Bed_Other, 0, 80, 48, 0, 0, 0\n\
             Shape00=0, 0, 0, 65535, 65535\n",
        );
        bus.on_region_changed(1, Some(&zone_a), 0, 0, None);
        let loops: Vec<_> = bus.active_loops().into_iter().map(|l| l.logical).collect();
        assert!(loops.iter().any(|n| n == "s_Bed_Test"));
        assert!(loops.iter().any(|n| n == "s_MusicComponent_CC12-01"));
        bus.on_region_changed(2, Some(&zone_b), 0, 0, None);
        let loops: Vec<_> = bus
            .active_loops()
            .into_iter()
            .map(|l| (l.logical, l.category, l.spatial))
            .collect();
        assert!(
            loops
                .iter()
                .all(|(n, _, _)| n != "s_Bed_Test" && n != "s_MusicComponent_CC12-01"),
            "old ambient/music must not survive region change: {loops:?}"
        );
        assert_eq!(loops.len(), 1);
        assert_eq!(loops[0].0, "s_Bed_Other");
        assert_eq!(loops[0].1, SoundCategory::Ambient);
        assert_eq!(loops[0].2.map(|s| s.radius), Some(48));
        assert_eq!(bus.active_zone(), Some(2));
        cleanup(&root);
    }

    /// Named falsifier: mute/disabled audio performs no backend playback.
    #[test]
    fn mute_performs_no_backend_playback() {
        let root = scratch_client(&["click.wav"]);
        let mut bus = AudioBus::null(&root);
        bus.set_settings(AudioSettings::silence());
        let mapped = bus.play(LogicalSoundEvent::oneshot(SoundCategory::Ui, "s_click"));
        assert_eq!(mapped, PlayOutcome::Muted);
        assert_eq!(bus.null_backend_plays(), 0);
        bus.set_settings(AudioSettings {
            disabled: true,
            muted: false,
            ..AudioSettings::default()
        });
        let mapped = bus.play(LogicalSoundEvent::oneshot(SoundCategory::Ui, "s_click"));
        assert_eq!(mapped, PlayOutcome::Disabled);
        assert_eq!(bus.null_backend_plays(), 0);
        cleanup(&root);
    }

    /// Named falsifier: repeated events do not leak sinks/threads/buffers or unbounded queues.
    #[test]
    fn repeated_events_do_not_leak() {
        let root = scratch_client(&["click.wav", "Bed_Test.wav"]);
        let mut bus = AudioBus::null(&root);
        let before_dir = bus.directory_resolves();
        for _ in 0..10_000u32 {
            let out = bus.play(LogicalSoundEvent::oneshot(SoundCategory::Ui, "s_click"));
            assert_eq!(out, PlayOutcome::Played);
        }
        assert_eq!(bus.null_backend_plays(), 10_000);
        assert_eq!(bus.active_loop_count(), 0, "oneshots must not stay owned");
        assert!(bus.resolve_cache_len() <= RESOLVE_CACHE_CAP);
        assert!(bus.decode_cache_len() <= DECODE_CACHE_CAP);
        assert!(bus.scheduled_len() <= SCHEDULE_CAP);
        assert!(bus.records().len() <= BUS_CALL_LOG_CAP);
        assert!(
            bus.directory_resolves() - before_dir <= 2,
            "pathological per-event directory walk: {}",
            bus.directory_resolves()
        );
        assert!(bus.resolve_hits() >= 9_000);
        for i in 0..2_000u32 {
            let _ = bus.play(LogicalSoundEvent::oneshot(
                SoundCategory::Ambient,
                format!("g_NoSuch_{i}"),
            ));
        }
        assert!(bus.resolve_cache_len() <= RESOLVE_CACHE_CAP);
        let _ = bus.play(LogicalSoundEvent::looping(
            SoundCategory::Ambient,
            "s_Bed_Test",
        ));
        let _ = bus.play(LogicalSoundEvent::looping(
            SoundCategory::Ambient,
            "s_Bed_Test",
        ));
        assert_eq!(bus.active_loop_count(), 1, "looping must dedup, not stack");
        cleanup(&root);
    }

    #[test]
    fn logout_clears_ambient_and_reconnect_stays_silent() {
        let root = scratch_client(&["Bed_Test.wav"]);
        let mut bus = AudioBus::null(&root);
        let zs = parse("[SoundRegion00];[x]\nSound00=2, s_Bed_Test, 0, 100, 24, 0, 0, 0\n");
        bus.on_region_changed(7, Some(&zs), 0, 0, None);
        assert_eq!(bus.active_loop_count(), 1);
        bus.on_logout();
        assert_eq!(bus.active_loop_count(), 0);
        assert!(bus.active_zone().is_none());
        bus.on_reconnect();
        assert_eq!(bus.active_loop_count(), 0);
        cleanup(&root);
    }

    #[test]
    fn spatial_params_and_tick_schedule_are_recorded() {
        let root = scratch_client(&["click.wav", "Bed_Test.wav"]);
        let mut bus = AudioBus::null(&root);
        let zs = parse(
            "[SoundRegion00];[terrain:test]\n\
             Sound00=2, s_Bed_Test, 0, 100, 24, 0, 0, 0\n\
             Sound01=1, s_click, 5, 80, 64, 0, 0, 0\n\
             Shape00=0, 0, 0, 100, 100\n",
        );
        bus.apply_zone_sounds(3, &zs, 0, 0, Some((10, 10)));
        assert_eq!(bus.active_loop_count(), 1);
        assert_eq!(bus.scheduled_len(), 1);
        assert_eq!(bus.null_backend_plays(), 1);
        bus.tick(5.0);
        assert_eq!(bus.scheduled_len(), 0);
        assert_eq!(bus.null_backend_plays(), 2);
        let click = bus
            .records()
            .into_iter()
            .find(|r| r.logical == "s_click")
            .expect("click");
        assert_eq!(click.spatial.map(|s| s.radius), Some(64));
        assert_eq!(click.category, SoundCategory::Ambient);
        cleanup(&root);
    }

    #[test]
    fn mapped_play_is_success_on_null_device() {
        let root = scratch_client(&["click.wav"]);
        let mut bus = AudioBus::null(&root);
        let out = bus.play(LogicalSoundEvent::oneshot(SoundCategory::Ui, "s_click"));
        assert_eq!(out, PlayOutcome::Played);
        assert!(out.is_success());
        assert_eq!(bus.device_kind(), AudioDeviceKind::Null);
        cleanup(&root);
    }

    #[test]
    fn settings_zero_music_stops_music_loop() {
        let root = scratch_client(&["MusicComponent_CC12-01.wav"]);
        let mut bus = AudioBus::null(&root);
        assert_eq!(
            bus.play(LogicalSoundEvent::looping(
                SoundCategory::Music,
                "s_MusicComponent_CC12-01"
            )),
            PlayOutcome::Played
        );
        assert_eq!(bus.active_loop_count(), 1);
        bus.set_settings(AudioSettings {
            music: 0.0,
            ..AudioSettings::default()
        });
        assert_eq!(bus.active_loop_count(), 0);
        let again = bus.play(LogicalSoundEvent::looping(
            SoundCategory::Music,
            "s_MusicComponent_CC12-01",
        ));
        assert_eq!(again, PlayOutcome::Muted);
        cleanup(&root);
    }

    /// Named falsifier: region/mute must stop category-owned one-shots, not only loops.
    #[test]
    fn stop_category_kills_oneshots_and_spatial_is_retained() {
        let root = scratch_client(&["click.wav"]);
        let mut bus = AudioBus::null(&root);
        let mut ev = LogicalSoundEvent::oneshot(SoundCategory::Ambient, "s_click");
        ev.spatial = Some(SpatialParams {
            x: 4000.0,
            y: 5000.0,
            z: 8000.0,
            radius: 2000,
        });
        assert_eq!(bus.play(ev), PlayOutcome::Played);
        assert_eq!(bus.active_oneshot_count(), 1);
        assert!(
            bus.active_loops().is_empty(),
            "oneshot must not be classified as a loop"
        );
        bus.set_listener(0.0, 0.0, 0.0);
        let rel = emitter_relative(
            [0.0, 0.0, 0.0],
            SpatialParams {
                x: 4000.0,
                y: 5000.0,
                z: 8000.0,
                radius: 2000,
            },
        );
        assert!(
            rel[0].abs() > 1.0,
            "listener-relative emitter must move with world offset, got {rel:?}"
        );
        bus.on_region_changed(1, None, 1200, 0, None);
        assert_eq!(
            bus.active_oneshot_count(),
            0,
            "ambient oneshot must die on region change"
        );
        cleanup(&root);
    }

    /// Named falsifier: stop_category must not stop other categories.
    #[test]
    fn stop_category_does_not_stop_other_categories() {
        let root = scratch_client(&["click.wav", "Bed_Test.wav"]);
        let mut bus = AudioBus::null(&root);
        assert_eq!(
            bus.play(LogicalSoundEvent::oneshot(SoundCategory::Combat, "s_click")),
            PlayOutcome::Played
        );
        assert_eq!(
            bus.play(LogicalSoundEvent::looping(
                SoundCategory::Spell,
                "s_Bed_Test"
            )),
            PlayOutcome::Played
        );
        assert_eq!(bus.oneshot_count_in(SoundCategory::Combat), 1);
        assert_eq!(bus.loop_count_in(SoundCategory::Spell), 1);
        bus.stop_category(SoundCategory::Combat);
        assert_eq!(bus.oneshot_count_in(SoundCategory::Combat), 0);
        assert_eq!(
            bus.loop_count_in(SoundCategory::Spell),
            1,
            "spell loop must survive combat stop"
        );
        cleanup(&root);
    }

    #[test]
    fn oneshot_voice_table_is_bounded() {
        let root = scratch_client(&["click.wav"]);
        let mut bus = AudioBus::null(&root);
        for _ in 0..(ONESHOT_SINK_CAP + 40) {
            let _ = bus.play(LogicalSoundEvent::oneshot(SoundCategory::Ui, "s_click"));
        }
        assert!(bus.active_oneshot_count() <= ONESHOT_SINK_CAP);
        assert!(bus.voice_table_len() <= ONESHOT_SINK_CAP + bus.active_loop_count());
        cleanup(&root);
    }

    #[test]
    fn logout_stops_combat_oneshots_not_claimed_as_null_product() {
        let root = scratch_client(&["click.wav"]);
        let mut bus = AudioBus::null(&root);
        assert_eq!(bus.device_kind(), AudioDeviceKind::Null);
        assert_eq!(
            bus.play(LogicalSoundEvent::oneshot(SoundCategory::Combat, "s_click")),
            PlayOutcome::Played
        );
        bus.on_logout();
        assert_eq!(bus.oneshot_count_in(SoundCategory::Combat), 0);
        cleanup(&root);
    }
}
