//! Safe semantic ingress for the opt-in native harness.
//!
//! This module converts the dependency-free wire DTO into existing product actions. It owns no
//! product state and cannot manufacture outcomes: callers must dispatch the returned action through
//! `preworld_product::dispatch_preworld_action` and the normal flow/HUD owners.

use crate::{preworld::PreWorldAction, preworld_camera, preworld_customize, preworld_options};
use caer_harness::{
    CameraControl as WireCamera, CommandEnvelope, CustomizerField as WireField, HarnessCommand,
    InputSource, Known, OptionsControl as WireOption, OptionsInteraction as WireInteraction,
    PreworldAction as WireAction, RefusalReason, Snapshot, WaitPredicate,
};
use std::{
    collections::BTreeMap,
    io::BufRead,
    sync::{Arc, Condvar, Mutex},
    thread,
};
use winit::event_loop::EventLoopProxy;
use winit::{event::MouseButton, keyboard::KeyCode};

pub const MAX_CACHED_COMMANDS: usize = 4096;
pub const MAX_PENDING_WAITS: usize = 256;
pub const MAX_COMMAND_LINE_BYTES: usize = 64 * 1024;
pub const MAX_PENDING_INGRESS_EVENTS: usize = 64;

#[derive(Debug)]
pub struct ReceivedCommand {
    pub envelope: CommandEnvelope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressFailure {
    Malformed,
    Oversized,
    ReadFailed,
}

impl IngressFailure {
    #[must_use]
    pub const fn safe_message(self) -> &'static str {
        match self {
            Self::Malformed => "harness command was malformed or invalid",
            Self::Oversized => "harness command exceeded the bounded line size",
            Self::ReadFailed => "harness stdin could not be read",
        }
    }
}

#[derive(Debug)]
pub enum HarnessUserEventKind {
    Command(ReceivedCommand),
    InvalidLine(IngressFailure),
    InputClosed,
}

/// A Winit user event with one bounded ingress credit. The permit is released when the event has
/// been handled (or Winit rejects it), so the stdin producer can never put more than
/// [`MAX_PENDING_INGRESS_EVENTS`] records into the event loop's otherwise-unbounded queue.
#[derive(Debug)]
pub struct HarnessUserEvent {
    pub kind: HarnessUserEventKind,
    _permit: Option<IngressPermit>,
}

impl HarnessUserEvent {
    fn untracked(kind: HarnessUserEventKind) -> Self {
        Self {
            kind,
            _permit: None,
        }
    }

    fn with_permit(mut self, permit: IngressPermit) -> Self {
        self._permit = Some(permit);
        self
    }
}

#[derive(Debug, Default)]
struct IngressLimiter {
    pending: Mutex<usize>,
    available: Condvar,
}

impl IngressLimiter {
    fn acquire(self: &Arc<Self>) -> IngressPermit {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        while *pending >= MAX_PENDING_INGRESS_EVENTS {
            pending = self
                .available
                .wait(pending)
                .unwrap_or_else(|e| e.into_inner());
        }
        *pending += 1;
        IngressPermit {
            limiter: Arc::clone(self),
        }
    }

    #[cfg(test)]
    fn pending(&self) -> usize {
        *self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(test)]
    fn try_acquire(self: &Arc<Self>) -> Option<IngressPermit> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if *pending >= MAX_PENDING_INGRESS_EVENTS {
            return None;
        }
        *pending += 1;
        Some(IngressPermit {
            limiter: Arc::clone(self),
        })
    }
}

struct IngressPermit {
    limiter: Arc<IngressLimiter>,
}

impl std::fmt::Debug for IngressPermit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("IngressPermit")
    }
}

impl Drop for IngressPermit {
    fn drop(&mut self) {
        let mut pending = self
            .limiter
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *pending = pending.saturating_sub(1);
        self.limiter.available.notify_one();
    }
}

pub fn spawn_stdin_reader(
    proxy: EventLoopProxy<HarnessUserEvent>,
) -> std::io::Result<thread::JoinHandle<()>> {
    let limiter = Arc::new(IngressLimiter::default());
    thread::Builder::new()
        .name("caer-harness-stdin".into())
        .spawn(move || {
            read_commands(std::io::stdin().lock(), |event| {
                let permit = limiter.acquire();
                proxy.send_event(event.with_permit(permit)).is_ok()
            });
        })
}

#[must_use]
pub fn decode_command_line(line: &[u8]) -> HarnessUserEvent {
    let Ok(line) = std::str::from_utf8(line) else {
        return HarnessUserEvent::untracked(HarnessUserEventKind::InvalidLine(
            IngressFailure::Malformed,
        ));
    };
    match serde_json::from_str::<CommandEnvelope>(line) {
        Ok(envelope) => match envelope.validate() {
            Ok(()) => HarnessUserEvent::untracked(HarnessUserEventKind::Command(ReceivedCommand {
                envelope,
            })),
            Err(_) => HarnessUserEvent::untracked(HarnessUserEventKind::InvalidLine(
                IngressFailure::Malformed,
            )),
        },
        Err(_) => HarnessUserEvent::untracked(HarnessUserEventKind::InvalidLine(
            IngressFailure::Malformed,
        )),
    }
}

/// Read newline-delimited commands without ever allocating in proportion to attacker-controlled
/// input. Oversized records are drained through a fixed buffer and become one typed failure.
pub fn read_commands<R: BufRead>(mut reader: R, mut send: impl FnMut(HarnessUserEvent) -> bool) {
    loop {
        let mut line = Vec::with_capacity(1024);
        let mut bounded = std::io::Read::take(&mut reader, (MAX_COMMAND_LINE_BYTES + 1) as u64);
        let read = match bounded.read_until(b'\n', &mut line) {
            Ok(read) => read,
            Err(_) => {
                let _ = send(HarnessUserEvent::untracked(
                    HarnessUserEventKind::InvalidLine(IngressFailure::ReadFailed),
                ));
                return;
            }
        };
        if read == 0 {
            let _ = send(HarnessUserEvent::untracked(
                HarnessUserEventKind::InputClosed,
            ));
            return;
        }
        let terminated = line.last() == Some(&b'\n');
        if line.len() > MAX_COMMAND_LINE_BYTES {
            if !terminated && !drain_line(&mut reader) {
                let _ = send(HarnessUserEvent::untracked(
                    HarnessUserEventKind::InvalidLine(IngressFailure::ReadFailed),
                ));
                return;
            }
            if !send(HarnessUserEvent::untracked(
                HarnessUserEventKind::InvalidLine(IngressFailure::Oversized),
            )) {
                return;
            }
            continue;
        }
        if terminated {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
        }
        if !send(decode_command_line(&line)) {
            return;
        }
    }
}

fn drain_line(reader: &mut impl BufRead) -> bool {
    loop {
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(_) => return false,
        };
        if available.is_empty() {
            return true;
        }
        if let Some(index) = available.iter().position(|byte| *byte == b'\n') {
            reader.consume(index + 1);
            return true;
        }
        let len = available.len();
        reader.consume(len);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CachedResponse {
    Ack {
        revision: u64,
    },
    Refusal {
        revision: u64,
        reason: RefusalReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheLookup {
    New,
    Duplicate(CachedResponse),
    Conflict,
    Full,
}

/// Result of the fail-closed checks that must run before a command can reach a product owner.
/// Duplicate lookup intentionally precedes deadline and revision checks: an identical retry must
/// replay its cached response even after the original action advanced the interaction revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightDecision {
    Execute,
    Replay(CachedResponse),
    Refuse(RefusalReason),
}

#[derive(Debug, Default)]
pub struct CommandCache {
    entries: BTreeMap<String, (CommandEnvelope, CachedResponse)>,
}

impl CommandCache {
    pub fn lookup(&self, command: &CommandEnvelope) -> CacheLookup {
        match self.entries.get(&command.command_id) {
            Some((previous, response)) if previous == command => {
                CacheLookup::Duplicate(response.clone())
            }
            Some(_) => CacheLookup::Conflict,
            None if self.entries.len() >= MAX_CACHED_COMMANDS => CacheLookup::Full,
            None => CacheLookup::New,
        }
    }

    pub fn remember(
        &mut self,
        command: CommandEnvelope,
        response: CachedResponse,
    ) -> Result<(), ()> {
        if self.entries.len() >= MAX_CACHED_COMMANDS
            || self.entries.contains_key(&command.command_id)
        {
            return Err(());
        }
        self.entries
            .insert(command.command_id.clone(), (command, response));
        Ok(())
    }
}

#[must_use]
pub fn preflight_command(
    command: &CommandEnvelope,
    active_run_id: &str,
    current_revision: u64,
    elapsed_ms: u64,
    cache: &CommandCache,
) -> PreflightDecision {
    if command.run_id != active_run_id {
        return PreflightDecision::Refuse(RefusalReason::Invalid);
    }
    match cache.lookup(command) {
        CacheLookup::Duplicate(response) => return PreflightDecision::Replay(response),
        CacheLookup::Conflict | CacheLookup::Full => {
            return PreflightDecision::Refuse(RefusalReason::Invalid);
        }
        CacheLookup::New => {}
    }
    if elapsed_ms >= command.deadline_ms {
        return PreflightDecision::Refuse(RefusalReason::DeadlineExpired);
    }
    if command.expected_revision != current_revision {
        return PreflightDecision::Refuse(RefusalReason::StaleRevision);
    }
    PreflightDecision::Execute
}

#[derive(Debug)]
pub struct PendingWait {
    pub command_id: String,
    pub predicate: WaitPredicate,
    pub deadline_ms: u64,
}

#[derive(Debug, Default)]
pub struct PendingWaits {
    waits: Vec<PendingWait>,
}

impl PendingWaits {
    pub fn push(&mut self, wait: PendingWait) -> Result<(), PendingWait> {
        if self.waits.len() >= MAX_PENDING_WAITS {
            Err(wait)
        } else {
            self.waits.push(wait);
            Ok(())
        }
    }

    pub fn take_satisfied(&mut self, snapshot: &Snapshot) -> Vec<String> {
        let mut satisfied = Vec::new();
        self.waits.retain(|wait| {
            if predicate_satisfied(&wait.predicate, snapshot) {
                satisfied.push(wait.command_id.clone());
                false
            } else {
                true
            }
        });
        satisfied
    }

    pub fn take_expired(&mut self, elapsed_ms: u64) -> Vec<String> {
        let mut expired = Vec::new();
        self.waits.retain(|wait| {
            if elapsed_ms >= wait.deadline_ms {
                expired.push(wait.command_id.clone());
                false
            } else {
                true
            }
        });
        expired
    }
}

#[must_use]
pub fn predicate_satisfied(predicate: &WaitPredicate, snapshot: &Snapshot) -> bool {
    match predicate {
        WaitPredicate::Screen { screen } => snapshot.session.screen.as_deref() == Some(screen),
        WaitPredicate::PendingRequest { request } => {
            snapshot.session.pending_request.as_deref() == Some(request)
        }
        WaitPredicate::SelectedIdentity { name, slot } => {
            snapshot.character.selected_name.as_deref() == Some(name)
                && snapshot.character.selected_protocol_slot == Some(*slot)
        }
        WaitPredicate::WorldReady => {
            snapshot.world.entered
                && snapshot.session.flow_step == "InWorld"
                && snapshot.session.screen.is_none()
                && snapshot.assets.terrain.state == "complete"
                && snapshot.assets.worker_queue_depth == (Known::Known { value: 0 })
        }
        WaitPredicate::AssetTerminal { job_id } => {
            let job = match job_id.as_str() {
                "terrain" => Some(&snapshot.assets.terrain),
                "preworld_scene" => Some(&snapshot.assets.preworld_scene),
                "texture" => Some(&snapshot.assets.texture),
                "mesh" => Some(&snapshot.assets.mesh),
                "animation" => Some(&snapshot.assets.animation),
                "gpu_upload" => Some(&snapshot.assets.gpu_upload),
                _ => None,
            };
            job.is_some_and(|job| matches!(job.state.as_str(), "complete" | "failed"))
        }
        WaitPredicate::PresentedAfter { counter } => {
            snapshot.presentation.presented_frames > *counter
        }
    }
}

#[must_use]
pub fn wait_predicate_supported(predicate: &WaitPredicate) -> bool {
    match predicate {
        WaitPredicate::Screen { screen } => matches!(
            screen.as_str(),
            "Login"
                | "Splash"
                | "Loading"
                | "RealmSelect"
                | "CharSelect"
                | "CharCreate"
                | "CharCustomize"
                | "CharStats"
        ),
        WaitPredicate::PendingRequest { request } => !request.trim().is_empty(),
        WaitPredicate::SelectedIdentity { name, slot } => {
            !name.trim().is_empty() && name.len() <= 20 && *slot <= 9
        }
        WaitPredicate::WorldReady | WaitPredicate::PresentedAfter { .. } => true,
        WaitPredicate::AssetTerminal { job_id } => matches!(
            job_id.as_str(),
            "terrain" | "preworld_scene" | "texture" | "mesh" | "animation" | "gpu_upload"
        ),
    }
}

pub fn adapt_preworld(action: WireAction) -> Result<PreWorldAction, RefusalReason> {
    Ok(match action {
        WireAction::LoginPlay => PreWorldAction::LoginPlay,
        WireAction::OpenSettings => PreWorldAction::OpenSettings,
        WireAction::LoginExit => PreWorldAction::LoginExit,
        WireAction::ChooseRealm { realm } => PreWorldAction::ChooseRealm(realm),
        WireAction::OpenCharCreate => PreWorldAction::OpenCharCreate,
        WireAction::CharCreateCancel => PreWorldAction::CharCreateCancel,
        WireAction::CharCreateContinue => PreWorldAction::CharCreateContinue,
        WireAction::CharCreateRandomName => PreWorldAction::CharCreateRandomName,
        WireAction::CharCreateFocusName => PreWorldAction::CharCreateFocusName,
        WireAction::CharCreateRace { index } => PreWorldAction::CharCreateRace(index),
        WireAction::CharCreateClass { index }
            if usize::from(index) < caer_protocol::creation_adapters::CLASS_ADAPTER_SLOTS =>
        {
            PreWorldAction::CharCreateClass(index)
        }
        WireAction::CharCreateClass { .. } => return Err(RefusalReason::Invalid),
        WireAction::CharCreateGender { gender } => PreWorldAction::CharCreateGender(gender),
        WireAction::SelectCharacterSlot { slot } => PreWorldAction::SelectCharacterSlot(slot),
        WireAction::EnterWorld => PreWorldAction::EnterWorld,
        WireAction::DeleteCharacter => PreWorldAction::DeleteCharacter,
        WireAction::BackToRealm => PreWorldAction::BackToRealm,
        WireAction::CharSelectQuit => PreWorldAction::CharSelectQuit,
        WireAction::CharSelectOptions => PreWorldAction::CharSelectOptions,
        WireAction::Options {
            control,
            interaction,
        } => PreWorldAction::Options(adapt_option(control, interaction)?),
        WireAction::QuitConfirmYes => PreWorldAction::QuitConfirmYes,
        WireAction::QuitConfirmNo => PreWorldAction::QuitConfirmNo,
        WireAction::DeleteConfirmYes => PreWorldAction::DeleteConfirmYes,
        WireAction::DeleteConfirmNo => PreWorldAction::DeleteConfirmNo,
        WireAction::CustomizeAdvance => PreWorldAction::CustomizeAdvance,
        WireAction::CustomizeStats => PreWorldAction::CustomizeStats,
        WireAction::CustomizeBack => PreWorldAction::CustomizeBack,
        WireAction::CustomizeCancel => PreWorldAction::CustomizeCancel,
        WireAction::CustomizeReset => PreWorldAction::CustomizeReset,
        WireAction::CustomizeRandom => PreWorldAction::CustomizeRandom,
        WireAction::CustomizeToggleLock { field } => PreWorldAction::CustomizeToggleLock {
            field: adapt_field(field),
        },
        WireAction::CustomizeAdjust { field, direction } => PreWorldAction::CustomizeAdjust {
            field: adapt_field(field),
            dir: direction,
        },
        WireAction::CustomizeSlider { field, tick } => PreWorldAction::CustomizeSlider {
            field: adapt_field(field),
            tick,
        },
        WireAction::CustomizeCamera { control } => {
            PreWorldAction::CustomizeCamera(adapt_camera(control))
        }
        WireAction::StatsAdjust { stat, direction } => PreWorldAction::StatsAdjust {
            stat,
            dir: direction,
        },
        WireAction::StatsReset => PreWorldAction::StatsReset,
        WireAction::StatsOptimize => PreWorldAction::StatsOptimize,
        WireAction::StatsDismiss => PreWorldAction::StatsDismiss,
    })
}

fn adapt_field(field: WireField) -> preworld_customize::CustomizerField {
    match field {
        WireField::Face => preworld_customize::CustomizerField::Face,
        WireField::Morph(slot) => preworld_customize::CustomizerField::Morph(slot),
        WireField::Mood => preworld_customize::CustomizerField::Mood,
        WireField::EyeColor => preworld_customize::CustomizerField::EyeColor,
        WireField::SkinTone => preworld_customize::CustomizerField::SkinTone,
        WireField::HairStyle => preworld_customize::CustomizerField::HairStyle,
        WireField::HairColor => preworld_customize::CustomizerField::HairColor,
        WireField::Tattoo => preworld_customize::CustomizerField::Tattoo,
        WireField::Size => preworld_customize::CustomizerField::Size,
    }
}

fn adapt_camera(control: WireCamera) -> preworld_camera::CameraControl {
    match control {
        WireCamera::Reset => preworld_camera::CameraControl::Reset,
        WireCamera::RotateLeft => preworld_camera::CameraControl::RotateLeft,
        WireCamera::RotateRight => preworld_camera::CameraControl::RotateRight,
        WireCamera::TiltUp => preworld_camera::CameraControl::TiltUp,
        WireCamera::TiltDown => preworld_camera::CameraControl::TiltDown,
        WireCamera::ZoomIn => preworld_camera::CameraControl::ZoomIn,
        WireCamera::ZoomOut => preworld_camera::CameraControl::ZoomOut,
    }
}

fn adapt_option(
    control: WireOption,
    interaction: WireInteraction,
) -> Result<preworld_options::OptionsHit, RefusalReason> {
    let id = adapt_option_id(control);
    match (id, interaction) {
        (preworld_options::OptionsId::Accept, WireInteraction::Accept) => {
            return Ok(preworld_options::OptionsHit::Accept);
        }
        (preworld_options::OptionsId::Cancel, WireInteraction::Cancel) => {
            return Ok(preworld_options::OptionsHit::Cancel);
        }
        (preworld_options::OptionsId::Accept | preworld_options::OptionsId::Cancel, _)
        | (_, WireInteraction::Accept | WireInteraction::Cancel) => {
            return Err(RefusalReason::Invalid);
        }
        _ => {}
    }
    let Some(row) = preworld_options::row_for(id) else {
        return Err(RefusalReason::Invalid);
    };
    if !row.enabled {
        return Err(RefusalReason::Unsupported);
    }
    let hit = match interaction {
        WireInteraction::Press
            if matches!(
                row.kind,
                preworld_options::RowKind::Check
                    | preworld_options::RowKind::Radio
                    | preworld_options::RowKind::Link
            ) =>
        {
            preworld_options::OptionsHit::Press(id)
        }
        WireInteraction::CycleLeft if row.kind == preworld_options::RowKind::Cycle => {
            preworld_options::OptionsHit::Cycle(id, preworld_options::CycleSide::Left)
        }
        WireInteraction::CycleRight if row.kind == preworld_options::RowKind::Cycle => {
            preworld_options::OptionsHit::Cycle(id, preworld_options::CycleSide::Right)
        }
        WireInteraction::Press | WireInteraction::CycleLeft | WireInteraction::CycleRight => {
            return Err(RefusalReason::Invalid);
        }
        WireInteraction::Accept | WireInteraction::Cancel => unreachable!("handled above"),
    };
    Ok(hit)
}

fn adapt_option_id(control: WireOption) -> preworld_options::OptionsId {
    use preworld_options::OptionsId as O;
    match control {
        WireOption::Resolution => O::Resolution,
        WireOption::ClipPlaneDistance => O::ClipPlaneDistance,
        WireOption::Windowed => O::Windowed,
        WireOption::FullScreenWindowed => O::FullScreenWindowed,
        WireOption::FullScreen => O::FullScreen,
        WireOption::Monitor => O::Monitor,
        WireOption::UseAtlantisTrees => O::UseAtlantisTrees,
        WireOption::UseAtlantisTerrain => O::UseAtlantisTerrain,
        WireOption::DynamicShadows => O::DynamicShadows,
        WireOption::ShadowQuality => O::ShadowQuality,
        WireOption::ShadowFigures => O::ShadowFigures,
        WireOption::ClassicWater => O::ClassicWater,
        WireOption::ShroudedIslesWater => O::ShroudedIslesWater,
        WireOption::ReflectiveWater => O::ReflectiveWater,
        WireOption::ReflectionQuality => O::ReflectionQuality,
        WireOption::ReflectionUpdate => O::ReflectionUpdate,
        WireOption::SleepMode => O::SleepMode,
        WireOption::ConfigureFigureVersions => O::ConfigureFigureVersions,
        WireOption::DefaultSettings => O::DefaultSettings,
        WireOption::BestVisualQuality => O::BestVisualQuality,
        WireOption::HighestFramerate => O::HighestFramerate,
        WireOption::ConfigureKeyboard => O::ConfigureKeyboard,
        WireOption::MouseMode => O::MouseMode,
        WireOption::MouselookSensitivity => O::MouselookSensitivity,
        WireOption::SkinChoice => O::SkinChoice,
        WireOption::UseClassicIcons => O::UseClassicIcons,
        WireOption::UseClassicNameFont => O::UseClassicNameFont,
        WireOption::MusicVolume => O::MusicVolume,
        WireOption::SoundVolume => O::SoundVolume,
        WireOption::AmbientMusicVolume => O::AmbientMusicVolume,
        WireOption::AmbientSoundVolume => O::AmbientSoundVolume,
        WireOption::Cancel => O::Cancel,
        WireOption::Accept => O::Accept,
    }
}

#[must_use]
pub fn synthetic_input_supported(command: &HarnessCommand) -> bool {
    matches!(
        command,
        HarnessCommand::Input {
            source: InputSource::SyntheticWindow,
            ..
        }
    )
}

pub fn adapt_pointer_button(button: &str) -> Result<MouseButton, RefusalReason> {
    match button {
        "left" => Ok(MouseButton::Left),
        "right" => Ok(MouseButton::Right),
        _ => Err(RefusalReason::Unsupported),
    }
}

/// Stable, layout-independent keys accepted by the synthetic adapter. The live binding map still
/// decides their meaning, and the Client refuses codes that do not resolve to a held action.
pub fn adapt_key_code(key: &str) -> Result<KeyCode, RefusalReason> {
    match key {
        "key_w" => Ok(KeyCode::KeyW),
        "key_a" => Ok(KeyCode::KeyA),
        "key_s" => Ok(KeyCode::KeyS),
        "key_d" => Ok(KeyCode::KeyD),
        "key_q" => Ok(KeyCode::KeyQ),
        "key_e" => Ok(KeyCode::KeyE),
        "shift_left" => Ok(KeyCode::ShiftLeft),
        "shift_right" => Ok(KeyCode::ShiftRight),
        "arrow_up" => Ok(KeyCode::ArrowUp),
        "arrow_down" => Ok(KeyCode::ArrowDown),
        _ => Err(RefusalReason::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caer_harness::{HarnessCommand, PreworldAction as W};
    use std::io::Cursor;

    #[test]
    fn semantic_action_mapping_is_exhaustive_and_preserves_parameters() {
        assert_eq!(
            adapt_preworld(W::ChooseRealm { realm: 3 }),
            Ok(PreWorldAction::ChooseRealm(3))
        );
        assert_eq!(
            adapt_preworld(W::CharCreateRace { index: 6 }),
            Ok(PreWorldAction::CharCreateRace(6))
        );
        assert_eq!(
            adapt_preworld(W::StatsAdjust {
                stat: 7,
                direction: -1
            }),
            Ok(PreWorldAction::StatsAdjust { stat: 7, dir: -1 })
        );
    }

    #[test]
    fn option_adapter_obeys_the_authoritative_row_kind_and_enabled_registry() {
        assert!(matches!(
            adapt_option(WireOption::Accept, WireInteraction::Accept),
            Ok(preworld_options::OptionsHit::Accept)
        ));
        assert_eq!(
            adapt_option(WireOption::Accept, WireInteraction::Press),
            Err(RefusalReason::Invalid)
        );
        assert!(matches!(
            adapt_option(WireOption::Resolution, WireInteraction::CycleRight),
            Ok(preworld_options::OptionsHit::Cycle(
                preworld_options::OptionsId::Resolution,
                preworld_options::CycleSide::Right
            ))
        ));
        assert_eq!(
            adapt_option(WireOption::Resolution, WireInteraction::Press),
            Err(RefusalReason::Invalid)
        );
        assert_eq!(
            adapt_option(WireOption::DynamicShadows, WireInteraction::Press),
            Err(RefusalReason::Unsupported)
        );
    }

    #[test]
    fn class_adapter_uses_the_canonical_product_slot_count() {
        let last = caer_protocol::creation_adapters::CLASS_ADAPTER_SLOTS as u8 - 1;
        assert_eq!(
            adapt_preworld(W::CharCreateClass { index: last }),
            Ok(PreWorldAction::CharCreateClass(last))
        );
        assert_eq!(
            adapt_preworld(W::CharCreateClass { index: last + 1 }),
            Err(RefusalReason::Invalid)
        );
    }

    #[test]
    fn duplicate_and_conflicting_ids_are_distinct() {
        let command = CommandEnvelope {
            schema: caer_harness::SCHEMA_V1.into(),
            run_id: "run".into(),
            command_id: "one".into(),
            expected_revision: 2,
            deadline_ms: 1000,
            command: HarnessCommand::Snapshot,
        };
        let mut cache = CommandCache::default();
        assert_eq!(cache.lookup(&command), CacheLookup::New);
        cache
            .remember(command.clone(), CachedResponse::Ack { revision: 2 })
            .unwrap();
        assert_eq!(
            cache.lookup(&command),
            CacheLookup::Duplicate(CachedResponse::Ack { revision: 2 })
        );
        let mut conflict = command.clone();
        conflict.command = HarnessCommand::Quit;
        assert_eq!(cache.lookup(&conflict), CacheLookup::Conflict);
    }

    #[test]
    fn duplicate_continue_replays_ack_without_a_second_side_effect() {
        let command = command("continue", 8, W::CustomizeAdvance);
        let mut cache = CommandCache::default();
        let mut dispatches = 0;

        assert_eq!(
            preflight_command(&command, "run", 8, 10, &cache),
            PreflightDecision::Execute
        );
        dispatches += 1;
        cache
            .remember(command.clone(), CachedResponse::Ack { revision: 9 })
            .unwrap();

        assert_eq!(
            preflight_command(&command, "run", 9, 999, &cache),
            PreflightDecision::Replay(CachedResponse::Ack { revision: 9 })
        );
        assert_eq!(
            dispatches, 1,
            "a duplicate must not re-enter product dispatch"
        );
    }

    #[test]
    fn stale_or_expired_command_never_reaches_dispatch() {
        let cache = CommandCache::default();
        let stale = command("stale", 4, W::CustomizeAdvance);
        assert_eq!(
            preflight_command(&stale, "run", 5, 1, &cache),
            PreflightDecision::Refuse(RefusalReason::StaleRevision)
        );

        let expired = command("expired", 5, W::CustomizeAdvance);
        assert_eq!(
            preflight_command(&expired, "run", 5, 1_001, &cache),
            PreflightDecision::Refuse(RefusalReason::DeadlineExpired)
        );
    }

    #[test]
    fn command_from_another_run_is_invalid() {
        let command = command("foreign", 0, W::LoginPlay);
        assert_eq!(
            preflight_command(&command, "different-run", 0, 0, &CommandCache::default()),
            PreflightDecision::Refuse(RefusalReason::Invalid)
        );
    }

    #[test]
    fn stdin_decoder_never_promotes_malformed_input() {
        assert!(matches!(
            decode_command_line(b"{not-json").kind,
            HarnessUserEventKind::InvalidLine(IngressFailure::Malformed)
        ));
    }

    #[test]
    fn reader_bounds_oversized_and_invalid_utf8_records_then_recovers() {
        let good = serde_json::to_vec(&command("good", 0, W::LoginPlay)).unwrap();
        let mut bytes = vec![b'x'; MAX_COMMAND_LINE_BYTES + 17];
        bytes.push(b'\n');
        bytes.extend_from_slice(&[0xff, b'\n']);
        bytes.extend_from_slice(&good);
        bytes.push(b'\n');
        let mut events = Vec::new();
        read_commands(Cursor::new(bytes), |event| {
            events.push(event);
            true
        });
        assert!(matches!(
            &events[0].kind,
            HarnessUserEventKind::InvalidLine(IngressFailure::Oversized)
        ));
        assert!(matches!(
            &events[1].kind,
            HarnessUserEventKind::InvalidLine(IngressFailure::Malformed)
        ));
        assert!(matches!(&events[2].kind, HarnessUserEventKind::Command(_)));
        assert!(matches!(&events[3].kind, HarnessUserEventKind::InputClosed));
    }

    #[test]
    fn ingress_credits_bound_the_otherwise_unbounded_winit_queue() {
        let limiter = Arc::new(IngressLimiter::default());
        let mut permits = (0..MAX_PENDING_INGRESS_EVENTS)
            .map(|_| limiter.acquire())
            .collect::<Vec<_>>();
        assert_eq!(limiter.pending(), MAX_PENDING_INGRESS_EVENTS);
        assert!(
            limiter.try_acquire().is_none(),
            "a flood must stop before it enters Winit's unbounded queue"
        );
        drop(permits.pop());
        assert_eq!(limiter.pending(), MAX_PENDING_INGRESS_EVENTS - 1);
        let replacement = limiter.acquire();
        assert_eq!(limiter.pending(), MAX_PENDING_INGRESS_EVENTS);
        drop(replacement);
    }

    #[test]
    fn deadline_is_absolute_on_the_run_clock_and_duplicate_replays_after_expiry() {
        let command = command("absolute", 0, W::LoginPlay);
        let mut cache = CommandCache::default();
        assert_eq!(
            preflight_command(&command, "run", 0, 1_000, &cache),
            PreflightDecision::Refuse(RefusalReason::DeadlineExpired)
        );
        cache
            .remember(command.clone(), CachedResponse::Ack { revision: 1 })
            .unwrap();
        assert_eq!(
            preflight_command(&command, "run", 99, 50_000, &cache),
            PreflightDecision::Replay(CachedResponse::Ack { revision: 1 })
        );
    }

    #[test]
    fn replay_cache_exhaustion_fails_closed_without_evicting_or_overwriting() {
        let mut cache = CommandCache::default();
        for index in 0..MAX_CACHED_COMMANDS {
            let command = command(&format!("id-{index}"), 0, W::LoginPlay);
            cache
                .remember(command, CachedResponse::Ack { revision: 1 })
                .unwrap();
        }
        let overflow = command("overflow", 0, W::LoginPlay);
        assert_eq!(cache.lookup(&overflow), CacheLookup::Full);
        assert!(cache
            .remember(overflow, CachedResponse::Ack { revision: 1 })
            .is_err());

        let original = command("id-0", 0, W::LoginPlay);
        let mut conflict = original.clone();
        conflict.command = HarnessCommand::Quit;
        assert!(cache
            .remember(
                conflict,
                CachedResponse::Refusal {
                    revision: 2,
                    reason: RefusalReason::Invalid,
                }
            )
            .is_err());
        assert_eq!(
            cache.lookup(&original),
            CacheLookup::Duplicate(CachedResponse::Ack { revision: 1 })
        );
    }

    #[test]
    fn waits_are_bounded_one_shot_and_deadline_owned() {
        let snapshot = snapshot("CharCreate", 3);
        let mut waits = PendingWaits::default();
        waits
            .push(PendingWait {
                command_id: "screen".into(),
                predicate: WaitPredicate::Screen {
                    screen: "CharCreate".into(),
                },
                deadline_ms: 50,
            })
            .unwrap();
        assert_eq!(waits.take_satisfied(&snapshot), vec!["screen"]);
        assert!(waits.take_satisfied(&snapshot).is_empty());

        waits
            .push(PendingWait {
                command_id: "late".into(),
                predicate: WaitPredicate::PresentedAfter { counter: 99 },
                deadline_ms: 50,
            })
            .unwrap();
        assert!(waits.take_expired(49).is_empty());
        assert_eq!(waits.take_expired(50), vec!["late"]);
    }

    #[test]
    fn world_ready_requires_flow_terrain_queue_and_no_loading_screen() {
        let mut ready = snapshot("Loading", 3);
        ready.world.entered = true;
        ready.session.flow_step = "Loading".into();
        ready.assets.terrain.state = "running".into();
        ready.assets.worker_queue_depth = Known::Known { value: 1 };
        assert!(!predicate_satisfied(&WaitPredicate::WorldReady, &ready));

        ready.session.flow_step = "InWorld".into();
        ready.session.screen = None;
        ready.assets.terrain.state = "complete".into();
        ready.assets.worker_queue_depth = Known::Known { value: 0 };
        assert!(predicate_satisfied(&WaitPredicate::WorldReady, &ready));
    }

    #[test]
    fn raw_input_accepts_every_declared_synthetic_kind_and_rejects_false_provenance() {
        use caer_harness::{InputAction, InputSource};
        let inputs = [
            InputAction::PointerMove { x: 1.0, y: 2.0 },
            InputAction::PointerButton {
                button: "left".into(),
                pressed: true,
            },
            InputAction::Wheel { x: 0.0, y: 1.0 },
            InputAction::Key {
                key: "key_w".into(),
                pressed: true,
            },
            InputAction::Text {
                text: "Name".into(),
            },
            InputAction::Focus { focused: true },
            InputAction::Resize {
                width: 1024,
                height: 768,
            },
        ];
        for input in inputs {
            assert!(synthetic_input_supported(&HarnessCommand::Input {
                input,
                source: InputSource::SyntheticWindow,
            }));
        }
        for source in [InputSource::Semantic, InputSource::ObservedPhysical] {
            assert!(!synthetic_input_supported(&HarnessCommand::Input {
                input: InputAction::Text {
                    text: "Name".into()
                },
                source,
            }));
        }
        assert_eq!(
            adapt_pointer_button("middle"),
            Err(RefusalReason::Unsupported)
        );
        assert_eq!(adapt_key_code("enter"), Err(RefusalReason::Unsupported));
    }

    fn command(id: &str, expected_revision: u64, action: W) -> CommandEnvelope {
        CommandEnvelope {
            schema: caer_harness::SCHEMA_V1.into(),
            run_id: "run".into(),
            command_id: id.into(),
            expected_revision,
            deadline_ms: 1_000,
            command: HarnessCommand::ActivatePreworld { action },
        }
    }

    fn snapshot(screen: &str, presented_frames: u64) -> Snapshot {
        serde_json::from_value(serde_json::json!({
            "revision": 1,
            "event_sequence": 1,
            "elapsed_ms": 1,
            "identity": {
                "build": {
                    "source_revision": {"state":"unknown","reason":"test"},
                    "dirty_diff_identity": {"state":"unknown","reason":"test"},
                    "executable_sha256": {"state":"unknown","reason":"test"},
                    "build_profile": {"state":"unknown","reason":"test"},
                    "adapter": {"state":"unknown","reason":"test"},
                    "backend": {"state":"unknown","reason":"test"},
                    "process_id": {"state":"unknown","reason":"test"},
                    "parent_process_id": {"state":"unknown","reason":"test"}
                },
                "lab": {"state":"unknown","reason":"test"},
                "server": {"state":"unknown","reason":"test"}
            },
            "session": {
                "phase":"test", "flow_step":"test", "screen":screen,
                "pending_request":null, "last_error":null,
                "connection":{"state":"unknown","reason":"test"},
                "inbound_queue_depth":{"state":"unknown","reason":"test"},
                "outbound_queue_depth":{"state":"unknown","reason":"test"},
                "account_realm":{"state":"unknown","reason":"test"}
            },
            "character": {
                "draft":{"name":"","realm":1,"race":1,"class_id":1,"gender":0,
                    "level":{"state":"unknown","reason":"test"},"stats":[0,0,0,0,0,0,0,0],
                    "face_type":0,"hair_style":0,"hair_color":0,"eye_color":0,"eye_size":0,
                    "lip_size":0,"custom_mode":0},
                "tattoo":{"value":0,"wire_status":{"state":"unknown","reason":"test"}},
                "locks":{"state":"known","value":[]},"overview":[],
                "selected_protocol_slot":null,"selected_name":null,"pending_create_name":null,
                "creation_sent":false
            },
            "world":{"region":{"state":"unknown","reason":"test"},
                "player_object_id":{"state":"unknown","reason":"test"},
                "position":{"state":"unknown","reason":"test"},
                "heading":{"state":"unknown","reason":"test"},"entered":false},
            "presentation":{"frame_attempts":0,"presented_frames":presented_frames,
                "last_present_age_ms":{"state":"unknown","reason":"test"},
                "surface_physical_size":{"state":"unknown","reason":"test"},
                "window_logical_size":{"state":"unknown","reason":"test"},
                "scale_factor":{"state":"unknown","reason":"test"},
                "display_mode":{"state":"unknown","reason":"test"},
                "focused":{"state":"unknown","reason":"test"},
                "minimized":{"state":"unknown","reason":"test"},
                "capture_active":false,"held_inputs":[]},
            "assets": {
                "terrain":{"state":"idle","started_elapsed_ms":{"state":"unknown","reason":"test"},"ended_elapsed_ms":{"state":"unknown","reason":"test"},"failure":null},
                "preworld_scene":{"state":"idle","started_elapsed_ms":{"state":"unknown","reason":"test"},"ended_elapsed_ms":{"state":"unknown","reason":"test"},"failure":null},
                "texture":{"state":"idle","started_elapsed_ms":{"state":"unknown","reason":"test"},"ended_elapsed_ms":{"state":"unknown","reason":"test"},"failure":null},
                "mesh":{"state":"idle","started_elapsed_ms":{"state":"unknown","reason":"test"},"ended_elapsed_ms":{"state":"unknown","reason":"test"},"failure":null},
                "animation":{"state":"idle","started_elapsed_ms":{"state":"unknown","reason":"test"},"ended_elapsed_ms":{"state":"unknown","reason":"test"},"failure":null},
                "gpu_upload":{"state":"idle","started_elapsed_ms":{"state":"unknown","reason":"test"},"ended_elapsed_ms":{"state":"unknown","reason":"test"},"failure":null},
                "worker_queue_depth":{"state":"unknown","reason":"test"},
                "stale_work_rejections":{"state":"unknown","reason":"test"}},
            "shutdown":{"state":"running","pending_exit":false,
                "live_commands_queued":{"state":"unknown","reason":"test"}}
        })).unwrap()
    }
}
