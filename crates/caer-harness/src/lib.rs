//! Versioned wire contracts shared by the CAER native client and development toolkit.
//!
//! This crate deliberately owns no processes and depends on no product crate.  It answers only
//! whether a command/event/evidence record is structurally safe to exchange.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fmt,
    path::{Component, Path},
};

pub const SCHEMA_V1: &str = "caer.harness/v1";
pub const EVENT_PREFIX: &str = "CAER_HARNESS\t";
pub const MAX_DEADLINE_MS: u64 = 300_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    UnsupportedSchema(String),
    EmptyField(&'static str),
    InvalidDeadline(u64),
    SensitiveField(String),
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchema(s) => write!(f, "unsupported harness schema `{s}`"),
            Self::EmptyField(field) => write!(f, "`{field}` must not be empty"),
            Self::InvalidDeadline(ms) => write!(
                f,
                "deadline_ms must be between 1 and {MAX_DEADLINE_MS}, got {ms}"
            ),
            Self::SensitiveField(path) => write!(f, "sensitive value is forbidden at `{path}`"),
        }
    }
}

impl std::error::Error for ValidationError {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandEnvelope {
    pub schema: String,
    pub run_id: String,
    pub command_id: String,
    /// Revision of the interaction/product preconditions observed by the caller. This revision
    /// changes when an action's meaning can change, never merely because a frame or clock advanced.
    pub expected_revision: u64,
    pub deadline_ms: u64,
    pub command: HarnessCommand,
}

impl CommandEnvelope {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_common(&self.schema, &self.run_id)?;
        require_nonempty("command_id", &self.command_id)?;
        if !(1..=MAX_DEADLINE_MS).contains(&self.deadline_ms) {
            return Err(ValidationError::InvalidDeadline(self.deadline_ms));
        }
        if let HarnessCommand::ActivatePreworld { action } = &self.command {
            action.validate()?;
        }
        if let HarnessCommand::Capture { artifact_name } = &self.command {
            validate_artifact_name(artifact_name)?;
        }
        if let HarnessCommand::Input { input, .. } = &self.command {
            input.validate()?;
        }
        reject_secrets(&serde_json::to_value(self).expect("command serialization"))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HarnessCommand {
    Capabilities,
    Snapshot,
    /// A runner-side subscription request. The native adapter registers the predicate and returns
    /// immediately; it must never block the Winit thread while waiting for satisfaction.
    Wait {
        predicate: WaitPredicate,
    },
    ActivatePreworld {
        action: PreworldAction,
    },
    Input {
        input: InputAction,
        source: InputSource,
    },
    Capture {
        artifact_name: String,
    },
    Quit,
}

/// Dependency-free DTO for every semantic action currently owned by `PreWorldAction`.
/// Phase 2 must adapt this enum exhaustively rather than accepting action names as strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PreworldAction {
    LoginPlay,
    OpenSettings,
    LoginExit,
    ChooseRealm {
        realm: u8,
    },
    OpenCharCreate,
    CharCreateCancel,
    CharCreateContinue,
    CharCreateRandomName,
    CharCreateFocusName,
    CharCreateRace {
        index: u8,
    },
    CharCreateClass {
        index: u8,
    },
    CharCreateGender {
        gender: u8,
    },
    SelectCharacterSlot {
        slot: u8,
    },
    EnterWorld,
    DeleteCharacter,
    BackToRealm,
    CharSelectQuit,
    CharSelectOptions,
    Options {
        control: OptionsControl,
        interaction: OptionsInteraction,
    },
    QuitConfirmYes,
    QuitConfirmNo,
    DeleteConfirmYes,
    DeleteConfirmNo,
    CustomizeAdvance,
    CustomizeStats,
    CustomizeBack,
    CustomizeCancel,
    CustomizeReset,
    CustomizeRandom,
    CustomizeToggleLock {
        field: CustomizerField,
    },
    CustomizeAdjust {
        field: CustomizerField,
        direction: i8,
    },
    CustomizeSlider {
        field: CustomizerField,
        tick: u8,
    },
    CustomizeCamera {
        control: CameraControl,
    },
    StatsAdjust {
        stat: u8,
        direction: i8,
    },
    StatsReset,
    StatsOptimize,
    StatsDismiss,
}

impl PreworldAction {
    fn validate(&self) -> Result<(), ValidationError> {
        let valid = match self {
            Self::ChooseRealm { realm } => (1..=3).contains(realm),
            Self::CharCreateRace { index } => *index <= 6,
            // The dependency-free wire crate cannot own the product's class-slot count. Its
            // adapter validates this index against `creation_adapters::CLASS_ADAPTER_SLOTS`.
            Self::CharCreateClass { .. } => true,
            Self::CharCreateGender { gender } => *gender <= 1,
            Self::SelectCharacterSlot { slot } => *slot <= 9,
            Self::CustomizeAdjust { field, direction } => {
                field.valid() && matches!(direction, -1 | 1)
            }
            Self::CustomizeSlider { field, tick } => field.slider_compatible() && *tick <= 8,
            Self::CustomizeToggleLock { field } => field.valid(),
            Self::StatsAdjust { stat, direction } => *stat <= 7 && matches!(direction, -1 | 1),
            _ => true,
        };
        if valid {
            Ok(())
        } else {
            Err(ValidationError::EmptyField("invalid preworld action value"))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "field", content = "slot", rename_all = "snake_case")]
pub enum CustomizerField {
    Face,
    Morph(u8),
    Mood,
    EyeColor,
    SkinTone,
    HairStyle,
    HairColor,
    Tattoo,
    Size,
}

impl CustomizerField {
    fn valid(self) -> bool {
        !matches!(self, Self::Morph(slot) if slot > 3)
    }
    fn slider_compatible(self) -> bool {
        matches!(self, Self::Morph(0..=3) | Self::Mood | Self::SkinTone)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraControl {
    Reset,
    RotateLeft,
    RotateRight,
    TiltUp,
    TiltDown,
    ZoomIn,
    ZoomOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionsInteraction {
    CycleLeft,
    CycleRight,
    Press,
    Accept,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionsControl {
    Resolution,
    ClipPlaneDistance,
    Windowed,
    FullScreenWindowed,
    FullScreen,
    Monitor,
    UseAtlantisTrees,
    UseAtlantisTerrain,
    DynamicShadows,
    ShadowQuality,
    ShadowFigures,
    ClassicWater,
    ShroudedIslesWater,
    ReflectiveWater,
    ReflectionQuality,
    ReflectionUpdate,
    SleepMode,
    ConfigureFigureVersions,
    DefaultSettings,
    BestVisualQuality,
    HighestFramerate,
    ConfigureKeyboard,
    MouseMode,
    MouselookSensitivity,
    SkinChoice,
    UseClassicIcons,
    UseClassicNameFont,
    MusicVolume,
    SoundVolume,
    AmbientMusicVolume,
    AmbientSoundVolume,
    Cancel,
    Accept,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WaitPredicate {
    Screen { screen: String },
    PendingRequest { request: String },
    SelectedIdentity { name: String, slot: u8 },
    WorldReady,
    AssetTerminal { job_id: String },
    PresentedAfter { counter: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputSource {
    Semantic,
    SyntheticWindow,
    ObservedPhysical,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputAction {
    PointerMove { x: f64, y: f64 },
    PointerButton { button: String, pressed: bool },
    Wheel { x: f32, y: f32 },
    Key { key: String, pressed: bool },
    Text { text: String },
    Focus { focused: bool },
    Resize { width: u32, height: u32 },
}

impl InputAction {
    fn validate(&self) -> Result<(), ValidationError> {
        let valid = match self {
            Self::PointerMove { x, y } => x.is_finite() && y.is_finite(),
            Self::PointerButton { button, .. } | Self::Key { key: button, .. } => {
                !button.is_empty() && button.len() <= 64 && button.is_ascii()
            }
            Self::Wheel { x, y } => x.is_finite() && y.is_finite(),
            Self::Text { text } => text.len() <= 4096,
            Self::Focus { .. } => true,
            Self::Resize { width, height } => *width > 0 && *height > 0,
        };
        valid
            .then_some(())
            .ok_or(ValidationError::EmptyField("invalid synthetic input value"))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventEnvelope {
    pub schema: String,
    pub run_id: String,
    pub sequence: u64,
    pub elapsed_ms: u64,
    pub event: HarnessEvent,
}

impl EventEnvelope {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_common(&self.schema, &self.run_id)?;
        if let HarnessEvent::Artifact { path, sha256, .. } = &self.event {
            validate_bundle_path(path)?;
            if sha256.len() != 64
                || !sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(ValidationError::EmptyField(
                    "artifact sha256 must be exactly 64 lowercase hexadecimal digits",
                ));
            }
        }
        reject_secrets(&serde_json::to_value(self).expect("event serialization"))
    }

    pub fn is_progress(&self) -> bool {
        !matches!(
            self.event,
            HarnessEvent::Heartbeat { .. } | HarnessEvent::Snapshot { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HarnessEvent {
    Hello {
        identity: HelloIdentity,
        features: Vec<String>,
    },
    Capabilities {
        commands: Vec<String>,
        observations: Vec<String>,
        proof_classes: Vec<String>,
        features: Vec<String>,
    },
    Ack {
        command_id: String,
        revision: u64,
        duplicate: bool,
    },
    Refusal {
        command_id: String,
        current_revision: u64,
        reason: RefusalReason,
    },
    StateTransition {
        revision: u64,
        from: String,
        to: String,
    },
    ServerEffectSummary {
        revision: u64,
        effect: String,
    },
    /// The app acquired a swapchain image and `present` returned. This is P3 presentation
    /// liveness only; it does not claim the compositor displayed the pixels (P4).
    FramePresented {
        revision: u64,
        counter: u64,
    },
    PredicateSatisfied {
        command_id: String,
        revision: u64,
    },
    AssetJob {
        revision: u64,
        job_id: String,
        state: String,
    },
    Heartbeat {
        revision: u64,
        progress_sequence: u64,
    },
    Snapshot {
        snapshot: Snapshot,
    },
    Artifact {
        artifact_id: String,
        path: String,
        sha256: String,
    },
    Fatal {
        classification: FatalClass,
        failure_id: String,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FatalClass {
    ProductFailure,
    AssertionFailure,
    DependencyMissing,
    OracleMissing,
    InfraFailure,
    Inconclusive,
}

impl FatalClass {
    #[must_use]
    pub const fn verdict(self) -> VerdictKind {
        match self {
            Self::ProductFailure => VerdictKind::ProductFailure,
            Self::AssertionFailure => VerdictKind::AssertionFailure,
            Self::DependencyMissing => VerdictKind::DependencyMissing,
            Self::OracleMissing => VerdictKind::OracleMissing,
            Self::InfraFailure => VerdictKind::InfraFailure,
            Self::Inconclusive => VerdictKind::Inconclusive,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Known<T> {
    Known { value: T },
    Unknown { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelloIdentity {
    pub source_revision: Known<String>,
    pub dirty_diff_identity: Known<String>,
    pub executable_sha256: Known<String>,
    pub build_profile: Known<String>,
    pub adapter: Known<String>,
    pub backend: Known<String>,
    pub process_id: Known<u32>,
    pub parent_process_id: Known<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    StaleRevision,
    WrongScreen,
    Unsupported,
    Invalid,
    DeadlineExpired,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub revision: u64,
    pub event_sequence: u64,
    pub elapsed_ms: u64,
    pub identity: SnapshotIdentity,
    pub session: SessionSnapshot,
    pub character: CharacterSnapshot,
    pub world: WorldSnapshot,
    pub presentation: PresentationSnapshot,
    pub assets: AssetSnapshot,
    pub shutdown: ShutdownSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotIdentity {
    pub build: HelloIdentity,
    pub lab: Known<String>,
    pub server: Known<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSnapshot {
    pub phase: String,
    pub flow_step: String,
    pub screen: Option<String>,
    pub pending_request: Option<String>,
    pub last_error: Option<String>,
    pub connection: Known<String>,
    pub inbound_queue_depth: Known<usize>,
    pub outbound_queue_depth: Known<usize>,
    pub account_realm: Known<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterSnapshot {
    pub draft: CharacterDraftSnapshot,
    pub tattoo: LocalOnlyU8,
    pub locks: Known<Vec<String>>,
    pub overview: Vec<OverviewCharacterSnapshot>,
    pub selected_protocol_slot: Option<u8>,
    pub selected_name: Option<String>,
    pub pending_create_name: Option<String>,
    pub creation_sent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterDraftSnapshot {
    pub name: String,
    pub realm: u8,
    pub race: u8,
    pub class_id: u8,
    pub gender: u8,
    pub level: Known<u8>,
    pub stats: [u8; 8],
    pub face_type: u8,
    pub hair_style: u8,
    pub hair_color: u8,
    pub eye_color: u8,
    pub eye_size: u8,
    pub lip_size: u8,
    pub custom_mode: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalOnlyU8 {
    pub value: u8,
    pub wire_status: Known<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverviewCharacterSnapshot {
    pub name: String,
    pub protocol_slot: u8,
    pub ui_slot: u8,
    pub realm: u8,
    pub race: u8,
    pub gender: u8,
    pub class_id: u8,
    pub region: u8,
    pub appearance_digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldSnapshot {
    pub region: Known<u16>,
    pub player_object_id: Known<u16>,
    pub position: Known<[f32; 3]>,
    pub heading: Known<u16>,
    pub entered: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresentationSnapshot {
    pub frame_attempts: u64,
    pub presented_frames: u64,
    pub last_present_age_ms: Known<u64>,
    pub surface_physical_size: Known<[u32; 2]>,
    pub window_logical_size: Known<[f64; 2]>,
    pub scale_factor: Known<f64>,
    pub display_mode: Known<String>,
    pub focused: Known<bool>,
    pub minimized: Known<bool>,
    pub capture_active: bool,
    pub held_inputs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetSnapshot {
    pub terrain: AssetJobSnapshot,
    pub preworld_scene: AssetJobSnapshot,
    pub texture: AssetJobSnapshot,
    pub mesh: AssetJobSnapshot,
    pub animation: AssetJobSnapshot,
    pub gpu_upload: AssetJobSnapshot,
    pub worker_queue_depth: Known<usize>,
    pub stale_work_rejections: Known<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetJobSnapshot {
    pub state: String,
    pub started_elapsed_ms: Known<u64>,
    pub ended_elapsed_ms: Known<u64>,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownSnapshot {
    pub state: String,
    pub pending_exit: bool,
    pub live_commands_queued: Known<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdempotencyDecision {
    First,
    IdenticalDuplicate,
    ConflictingReuse,
}

/// Pure reference contract for Phase 2's bounded acknowledgement cache.
#[derive(Debug, Default)]
pub struct CommandLedger {
    seen: BTreeMap<String, CommandEnvelope>,
}

impl CommandLedger {
    pub fn observe(&mut self, command: &CommandEnvelope) -> IdempotencyDecision {
        match self.seen.get(&command.command_id) {
            None => {
                self.seen
                    .insert(command.command_id.clone(), command.clone());
                IdempotencyDecision::First
            }
            Some(previous) if previous == command => IdempotencyDecision::IdenticalDuplicate,
            Some(_) => IdempotencyDecision::ConflictingReuse,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VerdictKind {
    Pass,
    ProductFailure,
    AssertionFailure,
    Timeout,
    DependencyMissing,
    OracleMissing,
    InfraFailure,
    Inconclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairPacket {
    pub failure_id: String,
    pub expected: String,
    pub observed: String,
    pub owner_paths: Vec<String>,
    pub next_discriminator: String,
    pub focused_gates: Vec<String>,
    pub artifacts: Vec<String>,
    pub blind_spots: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunVerdict {
    pub schema: String,
    pub run_id: String,
    pub result: VerdictKind,
    pub requested_proof_class: String,
    pub repair: Option<RepairPacket>,
    pub bundle_digest: Option<String>,
}

impl RunVerdict {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_common(&self.schema, &self.run_id)?;
        require_nonempty("requested_proof_class", &self.requested_proof_class)?;
        if !matches!(
            self.requested_proof_class.as_str(),
            "P1" | "P2" | "P3" | "P4" | "P5"
        ) {
            return Err(ValidationError::EmptyField(
                "requested_proof_class must be P1 through P5",
            ));
        }
        match (self.result, &self.repair) {
            (VerdictKind::Pass, None) => {}
            (VerdictKind::Pass, Some(_)) => {
                return Err(ValidationError::EmptyField(
                    "PASS must not contain a repair packet",
                ));
            }
            (_, None) => {
                return Err(ValidationError::EmptyField(
                    "non-PASS verdict must contain a repair packet",
                ));
            }
            (_, Some(repair)) => validate_repair_packet(repair)?,
        }
        if let Some(digest) = &self.bundle_digest {
            validate_sha256("bundle_digest", digest)?;
        }
        reject_secrets(&serde_json::to_value(self).expect("verdict serialization"))
    }
}

fn validate_repair_packet(repair: &RepairPacket) -> Result<(), ValidationError> {
    for (field, value) in [
        ("failure_id", repair.failure_id.as_str()),
        ("expected", repair.expected.as_str()),
        ("observed", repair.observed.as_str()),
        ("next_discriminator", repair.next_discriminator.as_str()),
    ] {
        require_nonempty(field, value)?;
    }
    for (field, values) in [
        ("owner_paths", &repair.owner_paths),
        ("focused_gates", &repair.focused_gates),
        ("artifacts", &repair.artifacts),
        ("blind_spots", &repair.blind_spots),
    ] {
        if values.is_empty() || values.iter().any(|value| value.trim().is_empty()) {
            return Err(ValidationError::EmptyField(field));
        }
    }
    Ok(())
}

fn validate_sha256(field: &'static str, value: &str) -> Result<(), ValidationError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(ValidationError::EmptyField(field))
    }
}

fn validate_common(schema: &str, run_id: &str) -> Result<(), ValidationError> {
    if schema != SCHEMA_V1 {
        return Err(ValidationError::UnsupportedSchema(schema.to_owned()));
    }
    require_nonempty("run_id", run_id)
}

fn require_nonempty(field: &'static str, value: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        Err(ValidationError::EmptyField(field))
    } else {
        Ok(())
    }
}

fn validate_artifact_name(name: &str) -> Result<(), ValidationError> {
    require_nonempty("artifact_name", name)?;
    if name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.starts_with('.')
    {
        return Err(ValidationError::EmptyField(
            "artifact_name must be a safe basename",
        ));
    }
    Ok(())
}

fn validate_bundle_path(path: &str) -> Result<(), ValidationError> {
    require_nonempty("artifact path", path)?;
    if path.contains('\\')
        || Path::new(path).is_absolute()
        || Path::new(path)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || !path.starts_with("frames/")
    {
        return Err(ValidationError::EmptyField(
            "artifact path must be under frames/ without traversal",
        ));
    }
    Ok(())
}

const SENSITIVE_KEYS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "credential",
];

pub fn reject_secrets(value: &Value) -> Result<(), ValidationError> {
    fn walk(value: &Value, path: &str) -> Result<(), ValidationError> {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    let child_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    let normalized = key.to_ascii_lowercase().replace('-', "_");
                    if SENSITIVE_KEYS
                        .iter()
                        .any(|needle| normalized.contains(needle))
                    {
                        return Err(ValidationError::SensitiveField(child_path));
                    }
                    walk(child, &child_path)?;
                }
            }
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    walk(child, &format!("{path}[{index}]"))?;
                }
            }
            Value::String(text) if looks_sensitive(text) => {
                return Err(ValidationError::SensitiveField(path.to_owned()));
            }
            _ => {}
        }
        Ok(())
    }
    walk(value, "")
}

fn looks_sensitive(text: &str) -> bool {
    redact_url_userinfo(&redact_assignments(text)) != text
}

pub fn redact_text(text: &str, literals: &[String]) -> String {
    let structured = text
        .lines()
        .map(redact_structured_log_line)
        .collect::<Vec<_>>()
        .join("\n");
    let mut redacted = redact_url_userinfo(&redact_assignments(&structured));
    redacted = literals
        .iter()
        .filter(|s| !s.is_empty())
        .fold(redacted, |out, secret| out.replace(secret, "[REDACTED]"));
    redacted
}

fn redact_structured_log_line(line: &str) -> String {
    let (prefix, payload) = ["stdout: ", "stderr: "]
        .into_iter()
        .find_map(|prefix| line.strip_prefix(prefix).map(|payload| (prefix, payload)))
        .unwrap_or(("", line));
    for start in std::iter::once(0).chain(
        payload
            .char_indices()
            .filter_map(|(index, ch)| matches!(ch, '{' | '[').then_some(index)),
    ) {
        let Ok(mut value) = serde_json::from_str::<Value>(&payload[start..]) else {
            continue;
        };
        redact_sensitive_values(&mut value);
        return format!(
            "{prefix}{}{}",
            &payload[..start],
            serde_json::to_string(&value).expect("JSON re-serialization")
        );
    }
    line.to_owned()
}

fn redact_sensitive_values(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let normalized = key.to_ascii_lowercase().replace('-', "_");
                if SENSITIVE_KEYS
                    .iter()
                    .any(|needle| normalized.contains(needle))
                {
                    *child = Value::String("[REDACTED]".into());
                } else {
                    redact_sensitive_values(child);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_sensitive_values),
        _ => {}
    }
}

fn redact_url_userinfo(text: &str) -> String {
    let mut out = text.to_owned();
    let mut cursor = 0;
    while let Some(relative_scheme) = out[cursor..].find("://") {
        let credentials_start = cursor + relative_scheme + 3;
        let authority_end = out[credentials_start..]
            .find(|ch: char| ch == '/' || ch.is_whitespace())
            .map(|offset| credentials_start + offset)
            .unwrap_or(out.len());
        if let Some(relative_at) = out[credentials_start..authority_end].rfind('@') {
            let at = credentials_start + relative_at;
            out.replace_range(credentials_start..at, "[REDACTED]");
            cursor = credentials_start + "[REDACTED]".len() + 1;
        } else {
            cursor = authority_end;
        }
        if cursor >= out.len() {
            break;
        }
    }
    out
}

fn redact_assignments(text: &str) -> String {
    const MARKERS: &[&str] = &[
        "password=",
        "passwd=",
        "secret=",
        "token=",
        "api_key=",
        "bearer ",
    ];
    let lower = text.to_ascii_lowercase();
    let mut cursor = 0;
    let mut out = String::with_capacity(text.len());
    while cursor < text.len() {
        let Some((relative_start, marker)) = MARKERS
            .iter()
            .filter_map(|marker| lower[cursor..].find(marker).map(|start| (start, *marker)))
            .min_by_key(|(start, _)| *start)
        else {
            break;
        };
        let start = cursor + relative_start;
        let value_start = start + marker.len();
        let value_len = text[value_start..]
            .find(|ch: char| ch.is_whitespace() || matches!(ch, '&' | ';' | ','))
            .unwrap_or(text.len() - value_start);
        let end = value_start + value_len;
        out.push_str(&text[cursor..value_start]);
        out.push_str("[REDACTED]");
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command() -> CommandEnvelope {
        CommandEnvelope {
            schema: SCHEMA_V1.into(),
            run_id: "run-one".into(),
            command_id: "command-one".into(),
            expected_revision: 7,
            deadline_ms: 1_000,
            command: HarnessCommand::Snapshot,
        }
    }

    #[test]
    fn command_round_trip_is_stable() {
        let value = serde_json::to_string(&command()).unwrap();
        let decoded: CommandEnvelope = serde_json::from_str(&value).unwrap();
        assert_eq!(decoded, command());
        decoded.validate().unwrap();
    }

    #[test]
    fn unknown_major_and_deadlines_fail_closed() {
        let mut bad = command();
        bad.schema = "caer.harness/v2".into();
        assert!(matches!(
            bad.validate(),
            Err(ValidationError::UnsupportedSchema(_))
        ));
        bad.schema = SCHEMA_V1.into();
        bad.deadline_ms = 0;
        assert_eq!(bad.validate(), Err(ValidationError::InvalidDeadline(0)));
        bad.deadline_ms = MAX_DEADLINE_MS + 1;
        assert!(matches!(
            bad.validate(),
            Err(ValidationError::InvalidDeadline(_))
        ));
    }

    #[test]
    fn serde_rejects_malformed_and_unknown_fields() {
        let mut value = serde_json::to_value(command()).unwrap();
        value["password"] = Value::String("nope".into());
        assert!(serde_json::from_value::<CommandEnvelope>(value).is_err());
        assert!(serde_json::from_str::<CommandEnvelope>("{not json").is_err());
    }

    #[test]
    fn secret_content_is_rejected_and_redacted() {
        let mut value = serde_json::to_value(command()).unwrap();
        value["command"] = serde_json::json!({
            "kind":"input",
            "input":{"kind":"text", "text":"password=hunter2"},
            "source":"semantic"
        });
        let decoded: CommandEnvelope = serde_json::from_value(value).unwrap();
        assert!(matches!(
            decoded.validate(),
            Err(ValidationError::SensitiveField(_))
        ));
        assert_eq!(
            redact_text("before hunter2 after", &["hunter2".into()]),
            "before [REDACTED] after"
        );
        assert_eq!(
            redact_text("password=hunter2 token=abcd done", &[]),
            "password=[REDACTED] token=[REDACTED] done"
        );
        assert_eq!(
            redact_text("connect tcp://alice:hunter2@example.test:10321/path", &[]),
            "connect tcp://[REDACTED]@example.test:10321/path"
        );
        let json = r#"stdout: {"password":"hunter2","nested":{"api_token":"abcd"},"ok":1}"#;
        let redacted = redact_text(json, &[]);
        assert!(!redacted.contains("hunter2"), "{redacted}");
        assert!(!redacted.contains("abcd"), "{redacted}");
        assert!(redacted.contains("[REDACTED]"), "{redacted}");
        let embedded = redact_text(
            r#"stdout: diagnostic payload={"password":"leakme-credential"}"#,
            &[],
        );
        assert!(!embedded.contains("leakme-credential"), "{embedded}");
        assert!(embedded.contains("[REDACTED]"), "{embedded}");
        reject_secrets(&Value::String("password=[REDACTED]".into())).unwrap();
        assert!(reject_secrets(&Value::String("password=still-secret".into())).is_err());
    }

    #[test]
    fn idempotency_and_revision_are_mandatory_wire_fields() {
        let value = serde_json::to_value(command()).unwrap();
        assert_eq!(value["command_id"], "command-one");
        assert_eq!(value["expected_revision"], 7);
        for field in ["command_id", "expected_revision"] {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<CommandEnvelope>(missing).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn typed_preworld_action_rejects_invalid_semantics() {
        let mut value = command();
        value.command = HarnessCommand::ActivatePreworld {
            action: PreworldAction::ChooseRealm { realm: 4 },
        };
        assert!(value.validate().is_err());
        value.command = HarnessCommand::ActivatePreworld {
            action: PreworldAction::CustomizeSlider {
                field: CustomizerField::HairStyle,
                tick: 3,
            },
        };
        assert!(value.validate().is_err());
        value.command = HarnessCommand::ActivatePreworld {
            action: PreworldAction::CustomizeSlider {
                field: CustomizerField::Morph(2),
                tick: 8,
            },
        };
        value.validate().unwrap();
    }

    #[test]
    fn capture_name_cannot_escape_the_bundle() {
        for name in [
            "../frame.png",
            "/tmp/frame.png",
            "sub/frame.png",
            "sub\\frame.png",
            ".hidden",
        ] {
            let mut value = command();
            value.command = HarnessCommand::Capture {
                artifact_name: name.into(),
            };
            assert!(value.validate().is_err(), "accepted {name}");
        }
        let mut value = command();
        value.command = HarnessCommand::Capture {
            artifact_name: "frame-001.png".into(),
        };
        value.validate().unwrap();
    }

    #[test]
    fn command_id_reuse_distinguishes_identical_from_conflicting_payloads() {
        let original = command();
        let mut ledger = CommandLedger::default();
        assert_eq!(ledger.observe(&original), IdempotencyDecision::First);
        assert_eq!(
            ledger.observe(&original),
            IdempotencyDecision::IdenticalDuplicate
        );
        let mut conflict = original.clone();
        conflict.command = HarnessCommand::Quit;
        assert_eq!(
            ledger.observe(&conflict),
            IdempotencyDecision::ConflictingReuse
        );
    }

    #[test]
    fn artifact_events_refuse_traversal_and_noncanonical_digests() {
        let event = |path: &str, sha256: &str| EventEnvelope {
            schema: SCHEMA_V1.into(),
            run_id: "artifact-test".into(),
            sequence: 1,
            elapsed_ms: 1,
            event: HarnessEvent::Artifact {
                artifact_id: "frame-one".into(),
                path: path.into(),
                sha256: sha256.into(),
            },
        };
        let digest = "a".repeat(64);
        event("frames/frame-one.png", &digest).validate().unwrap();
        for path in [
            "../../outside",
            "/tmp/outside",
            "frames/../outside",
            "frames\\outside",
        ] {
            assert!(event(path, &digest).validate().is_err(), "accepted {path}");
        }
        for bad in ["abc", &"A".repeat(64), &"g".repeat(64)] {
            assert!(
                event("frames/frame-one.png", bad).validate().is_err(),
                "accepted {bad}"
            );
        }
    }

    #[test]
    fn verdict_shape_cannot_launder_failure_or_invent_a_repair_for_pass() {
        let repair = RepairPacket {
            failure_id: "TEST.FAILURE".into(),
            expected: "expected state".into(),
            observed: "observed state".into(),
            owner_paths: vec!["owner.rs".into()],
            next_discriminator: "run the focused control".into(),
            focused_gates: vec!["cargo test focused".into()],
            artifacts: vec!["events.jsonl".into()],
            blind_spots: vec!["native presentation not observed".into()],
        };
        let mut verdict = RunVerdict {
            schema: SCHEMA_V1.into(),
            run_id: "verdict-shape".into(),
            result: VerdictKind::Pass,
            requested_proof_class: "P1".into(),
            repair: None,
            bundle_digest: Some("a".repeat(64)),
        };
        verdict.validate().unwrap();
        verdict.repair = Some(repair.clone());
        assert!(verdict.validate().is_err());
        verdict.result = VerdictKind::ProductFailure;
        verdict.repair = None;
        assert!(verdict.validate().is_err());
        verdict.repair = Some(repair);
        verdict.bundle_digest = Some("not-a-digest".into());
        assert!(verdict.validate().is_err());
        verdict.bundle_digest = Some("b".repeat(64));
        verdict.validate().unwrap();
    }

    #[test]
    fn unknown_is_explicit_and_survives_wire_round_trip() {
        let unknown = Known::<u8>::Unknown {
            reason: "realm has not been established".into(),
        };
        let encoded = serde_json::to_string(&unknown).unwrap();
        assert_eq!(
            serde_json::from_str::<Known<u8>>(&encoded).unwrap(),
            unknown
        );
        assert!(encoded.contains("\"state\":\"unknown\""));
        assert!(!encoded.contains("\"value\":0"));
    }
}
