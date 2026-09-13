//! Opt-in native telemetry for the player application.
//!
//! This module is deliberately write-only with respect to the harness: it serializes immutable
//! observations borrowed from product owners. Action ingress belongs to Toolkit Phase 2.

use caer_harness::{
    EventEnvelope, HarnessEvent, HelloIdentity, Known, Snapshot, EVENT_PREFIX, SCHEMA_V1,
};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path, time::Instant};

#[must_use]
pub fn project_character_snapshot(
    draft: &caer_protocol::charcreate::CharacterCreateDraft,
    customizer: crate::preworld_product::CustomizerState,
    overview: Option<&caer_protocol::overview::CharacterOverview>,
    selected_protocol_slot: Option<u8>,
    pending_create_name: Option<&str>,
    creation_sent: bool,
) -> caer_harness::CharacterSnapshot {
    use crate::preworld_customize::CustomizerField;
    use caer_harness::{CharacterDraftSnapshot, CharacterSnapshot, Known, LocalOnlyU8};

    let overview_rows = overview
        .map(|overview| {
            overview
                .characters
                .iter()
                .map(|character| {
                    let (race, db_gender) =
                        caer_protocol::overview::decode_overview_race_gender(character.race_gender);
                    let mut hasher = Sha256::new();
                    hasher.update(format!("{:?}", character.appearance()).as_bytes());
                    caer_harness::OverviewCharacterSnapshot {
                        name: character.name.clone(),
                        protocol_slot: character.slot,
                        ui_slot: character.slot,
                        realm: character.realm,
                        race,
                        gender: caer_protocol::overview::fig3_gender_from_db(db_gender),
                        class_id: character.class_id,
                        region: character.region,
                        appearance_digest: format!("{:x}", hasher.finalize()),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let selected_name = selected_protocol_slot.and_then(|slot| {
        overview?
            .characters
            .iter()
            .find_map(|character| (character.slot == slot).then(|| character.name.clone()))
    });
    let fields = [
        (CustomizerField::Face, "face"),
        (CustomizerField::Morph(0), "morph_0"),
        (CustomizerField::Morph(1), "morph_1"),
        (CustomizerField::Morph(2), "morph_2"),
        (CustomizerField::Morph(3), "morph_3"),
        (CustomizerField::Mood, "mood"),
        (CustomizerField::EyeColor, "eye_color"),
        (CustomizerField::SkinTone, "skin_tone"),
        (CustomizerField::HairStyle, "hair_style"),
        (CustomizerField::HairColor, "hair_color"),
        (CustomizerField::Tattoo, "tattoo"),
        (CustomizerField::Size, "size"),
    ];
    CharacterSnapshot {
        draft: CharacterDraftSnapshot {
            name: draft.name.clone(),
            realm: draft.realm,
            race: draft.race,
            class_id: draft.class_id,
            gender: draft.gender,
            level: Known::Unknown {
                reason: "create draft has no client-owned level field".into(),
            },
            stats: draft.stats,
            face_type: draft.face_type,
            hair_style: draft.hair_style,
            hair_color: draft.hair_color,
            eye_color: draft.eye_color,
            eye_size: draft.eye_size,
            lip_size: draft.lip_size,
            custom_mode: draft.custom_mode,
        },
        tattoo: LocalOnlyU8 {
            value: customizer.tattoo_index(),
            wire_status: Known::Unknown {
                reason: "tattoo is explicitly local-only; wire owner is unproven".into(),
            },
        },
        locks: Known::Known {
            value: fields
                .into_iter()
                .filter_map(|(field, name)| customizer.is_locked(field).then(|| name.into()))
                .collect(),
        },
        overview: overview_rows,
        selected_protocol_slot,
        selected_name,
        pending_create_name: pending_create_name.map(str::to_owned),
        creation_sent,
    }
}

pub struct NativeHarness {
    run_id: String,
    bundle_root: Option<std::path::PathBuf>,
    started: Instant,
    sequence: u64,
    revision: u64,
    progress_sequence: u64,
    last_heartbeat: Instant,
    #[cfg(test)]
    emitted: Vec<EventEnvelope>,
}

impl NativeHarness {
    pub fn new(run_id: String) -> Result<Self, String> {
        if run_id.trim().is_empty() {
            return Err("--harness-jsonl requires a non-empty run ID".into());
        }
        caer_harness::reject_secrets(&serde_json::Value::String(run_id.clone()))
            .map_err(|error| error.to_string())?;
        let now = Instant::now();
        Ok(Self {
            run_id,
            bundle_root: std::env::var_os("CAER_HARNESS_BUNDLE_DIR").map(std::path::PathBuf::from),
            started: now,
            sequence: 0,
            revision: 0,
            progress_sequence: 0,
            last_heartbeat: now,
            #[cfg(test)]
            emitted: Vec::new(),
        })
    }

    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    #[must_use]
    pub fn next_sequence(&self) -> u64 {
        self.sequence + 1
    }

    #[must_use]
    pub fn elapsed_ms(&self) -> u64 {
        self.started
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    pub fn hello(&mut self, identity: HelloIdentity, features: Vec<String>) {
        self.emit(HarnessEvent::Hello { identity, features }, true);
    }

    pub fn capabilities(&mut self) {
        self.emit(
            HarnessEvent::Capabilities {
                commands: vec![
                    "capabilities".into(),
                    "snapshot".into(),
                    "wait".into(),
                    "activate_preworld".into(),
                    "input.synthetic_text_focus_resize".into(),
                    "capture".into(),
                    "quit".into(),
                ],
                observations: vec![
                    "authoritative_snapshot".into(),
                    "state_transition".into(),
                    "server_effect_summary".into(),
                    "frame_presented".into(),
                    "asset_job".into(),
                    "artifact".into(),
                    "shutdown".into(),
                ],
                proof_classes: vec!["P1".into(), "P2".into(), "P3".into()],
                features: vec!["revision_checked".into(), "idempotent".into()],
            },
            true,
        );
    }

    pub fn transition(&mut self, from: String, to: String) {
        if from == to {
            return;
        }
        self.revision = self.revision.saturating_add(1);
        self.emit(
            HarnessEvent::StateTransition {
                revision: self.revision,
                from,
                to,
            },
            true,
        );
    }

    pub fn asset_job(&mut self, job_id: String, state: String) {
        self.emit(
            HarnessEvent::AssetJob {
                revision: self.revision,
                job_id,
                state,
            },
            true,
        );
    }

    pub fn server_effect_summary(&mut self, effect: String) {
        self.emit(
            HarnessEvent::ServerEffectSummary {
                revision: self.revision,
                effect,
            },
            true,
        );
    }

    pub fn frame_presented(&mut self, counter: u64) {
        self.emit(
            HarnessEvent::FramePresented {
                revision: self.revision,
                counter,
            },
            true,
        );
    }

    pub fn ack(&mut self, command_id: String, revision: u64, duplicate: bool) {
        self.emit(
            HarnessEvent::Ack {
                command_id,
                revision,
                duplicate,
            },
            true,
        );
    }

    pub fn refusal(
        &mut self,
        command_id: String,
        revision: u64,
        reason: caer_harness::RefusalReason,
    ) {
        self.emit(
            HarnessEvent::Refusal {
                command_id,
                current_revision: revision,
                reason,
            },
            true,
        );
    }

    pub fn predicate_satisfied(&mut self, command_id: String) {
        self.emit(
            HarnessEvent::PredicateSatisfied {
                command_id,
                revision: self.revision,
            },
            true,
        );
    }

    pub fn artifact_file(&mut self, artifact_id: String, path: &Path) -> Result<(), String> {
        let relative = self.bundle_root.as_ref().map_or_else(
            || Ok(path),
            |root| {
                path.strip_prefix(root)
                    .map_err(|_| "artifact is outside the configured harness bundle".to_owned())
            },
        )?;
        let bundle_path = relative
            .to_str()
            .ok_or_else(|| "artifact path is not UTF-8".to_owned())?
            .to_owned();
        let sha256 = hash_file(path)?;
        let event = HarnessEvent::Artifact {
            artifact_id,
            path: bundle_path,
            sha256,
        };
        let probe = EventEnvelope {
            schema: SCHEMA_V1.into(),
            run_id: self.run_id.clone(),
            sequence: self.next_sequence(),
            elapsed_ms: self.elapsed_ms(),
            event: event.clone(),
        };
        probe.validate().map_err(|error| error.to_string())?;
        self.emit(event, true);
        Ok(())
    }

    #[must_use]
    pub fn capture_request_path(&self) -> std::path::PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(self.run_id.as_bytes());
        let digest = format!("{:x}", hasher.finalize());
        std::env::temp_dir().join(format!("caer-record-request-{}", &digest[..16]))
    }

    pub fn snapshot(&mut self, mut snapshot: Snapshot) {
        snapshot.revision = self.revision;
        snapshot.event_sequence = self.next_sequence();
        snapshot.elapsed_ms = self.elapsed_ms();
        self.emit(HarnessEvent::Snapshot { snapshot }, false);
    }

    pub fn heartbeat_if_due(&mut self) -> bool {
        if self.last_heartbeat.elapsed() < std::time::Duration::from_secs(1) {
            return false;
        }
        self.last_heartbeat = Instant::now();
        self.emit(
            HarnessEvent::Heartbeat {
                revision: self.revision,
                progress_sequence: self.progress_sequence,
            },
            false,
        );
        true
    }

    pub fn fatal(
        &mut self,
        classification: caer_harness::FatalClass,
        failure_id: &str,
        message: String,
    ) {
        self.emit(
            HarnessEvent::Fatal {
                classification,
                failure_id: failure_id.into(),
                message,
            },
            true,
        );
    }

    fn emit(&mut self, event: HarnessEvent, progress: bool) {
        self.sequence = self.sequence.saturating_add(1);
        if progress {
            self.progress_sequence = self.sequence;
        }
        let envelope = EventEnvelope {
            schema: SCHEMA_V1.into(),
            run_id: self.run_id.clone(),
            sequence: self.sequence,
            elapsed_ms: self.elapsed_ms(),
            event,
        };
        let envelope = if let Err(error) = envelope.validate() {
            eprintln!("rustdaoc: harness refused unsafe telemetry: {error}");
            EventEnvelope {
                schema: SCHEMA_V1.into(),
                run_id: self.run_id.clone(),
                sequence: self.sequence,
                elapsed_ms: self.elapsed_ms(),
                event: HarnessEvent::Fatal {
                    classification: caer_harness::FatalClass::InfraFailure,
                    failure_id: "native.telemetry_validation".into(),
                    message: "native telemetry was rejected before emission".into(),
                },
            }
        } else {
            envelope
        };
        debug_assert!(envelope.validate().is_ok());
        #[cfg(test)]
        self.emitted.push(envelope.clone());
        let json = serde_json::to_string(&envelope).expect("validated harness event serializes");
        println!("{EVENT_PREFIX}{json}");
    }
}

#[must_use]
pub fn hello_process_identity() -> HelloIdentity {
    HelloIdentity {
        source_revision: Known::Known {
            value: caer_client::evidence::BUILD_COMMIT.into(),
        },
        dirty_diff_identity: match caer_client::evidence::worktree_dirty_now() {
            Some(dirty) => Known::Known {
                value: if dirty { "dirty" } else { "clean" }.into(),
            },
            None => Known::Unknown {
                reason: "git worktree state unavailable".into(),
            },
        },
        executable_sha256: executable_sha256(),
        build_profile: Known::Known {
            value: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .into(),
        },
        adapter: Known::Unknown {
            reason: "GPU has not initialized".into(),
        },
        backend: Known::Unknown {
            reason: "GPU has not initialized".into(),
        },
        process_id: Known::Known {
            value: std::process::id(),
        },
        parent_process_id: parent_process_id(),
    }
}

pub fn bind_gpu_identity(identity: &mut HelloIdentity, adapter: String, backend: String) {
    identity.adapter = Known::Known { value: adapter };
    identity.backend = Known::Known { value: backend };
}

fn executable_sha256() -> Known<String> {
    let path = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            return Known::Unknown {
                reason: error.to_string(),
            }
        }
    };
    match hash_file(&path) {
        Ok(value) => Known::Known { value },
        Err(reason) => Known::Unknown { reason },
    }
}

fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let mut buffer = [0_u8; 64 * 1024];
    let mut hasher = Sha256::new();
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(target_os = "linux")]
fn parent_process_id() -> Known<u32> {
    Known::Known {
        value: unsafe { libc::getppid() as u32 },
    }
}

#[cfg(not(target_os = "linux"))]
fn parent_process_id() -> Known<u32> {
    Known::Unknown {
        reason: "parent process identity is not implemented on this platform".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_requires_safe_explicit_run_identity() {
        assert!(NativeHarness::new("native-one".into()).is_ok());
        assert!(NativeHarness::new("".into()).is_err());
        assert!(NativeHarness::new("password=hunter2".into()).is_err());
    }

    #[test]
    fn heartbeat_is_liveness_but_not_progress_or_revision() {
        let mut harness = NativeHarness::new("heartbeat-control".into()).unwrap();
        harness.frame_presented(1);
        let progress = harness.progress_sequence;
        let revision = harness.revision();
        harness.last_heartbeat = Instant::now() - std::time::Duration::from_secs(2);
        assert!(harness.heartbeat_if_due());

        let event = harness.emitted.last().unwrap();
        assert!(matches!(event.event, HarnessEvent::Heartbeat { .. }));
        assert!(!event.is_progress());
        assert_eq!(harness.progress_sequence, progress);
        assert_eq!(harness.revision(), revision);
    }

    #[test]
    fn unsafe_owner_text_becomes_a_typed_infra_failure_without_leaking() {
        let mut harness = NativeHarness::new("secret-control".into()).unwrap();
        harness.server_effect_summary("password=hunter2".into());
        let event = harness.emitted.last().unwrap();
        assert!(matches!(
            event.event,
            HarnessEvent::Fatal {
                classification: caer_harness::FatalClass::InfraFailure,
                ref failure_id,
                ..
            } if failure_id == "native.telemetry_validation"
        ));
        assert!(!serde_json::to_string(event).unwrap().contains("hunter2"));
    }

    #[test]
    fn product_fatal_cannot_be_laundered_as_toolkit_infrastructure() {
        let mut harness = NativeHarness::new("product-failure-control".into()).unwrap();
        harness.fatal(
            caer_harness::FatalClass::ProductFailure,
            "native.surface_validation",
            "surface failed".into(),
        );
        assert!(matches!(
            harness.emitted.last().unwrap().event,
            HarnessEvent::Fatal {
                classification: caer_harness::FatalClass::ProductFailure,
                ..
            }
        ));
    }

    #[test]
    fn revisions_advance_only_for_meaningful_native_events() {
        let mut harness = NativeHarness::new("revision-control".into()).unwrap();
        assert_eq!(harness.revision(), 0);
        harness.frame_presented(1);
        assert_eq!(harness.revision(), 0);
        harness.transition("login".into(), "realm_select".into());
        assert_eq!(harness.revision(), 1);
        harness.asset_job("preworld".into(), "complete".into());
        assert_eq!(harness.revision(), 1);
        harness.server_effect_summary("overview=1".into());
        assert_eq!(harness.revision(), 1);
    }

    #[test]
    fn acknowledgements_preserve_the_command_revision_and_capabilities_are_explicit() {
        let mut harness = NativeHarness::new("phase2-contract".into()).unwrap();
        harness.transition("a".into(), "b".into());
        harness.ack("original".into(), 1, false);
        harness.transition("b".into(), "c".into());
        harness.ack("original".into(), 1, true);
        harness.capabilities();
        assert!(matches!(
            harness.emitted[1].event,
            HarnessEvent::Ack {
                revision: 1,
                duplicate: false,
                ..
            }
        ));
        assert!(matches!(
            harness.emitted[3].event,
            HarnessEvent::Ack {
                revision: 1,
                duplicate: true,
                ..
            }
        ));
        assert!(matches!(
            harness.emitted[4].event,
            HarnessEvent::Capabilities { ref commands, .. }
                if commands.iter().any(|command| command == "activate_preworld")
        ));
    }

    #[test]
    fn character_projection_equals_authoritative_product_owners_and_is_read_only() {
        use crate::preworld::PreWorldAction;
        use crate::preworld_customize::CustomizerField;
        use crate::preworld_product::{dispatch_preworld_action, PreWorldProductState};

        let mut owner = PreWorldProductState::default();
        owner.create_draft.name = "Harnesshero".into();
        owner.create_draft.face_type = 3;
        owner.create_draft.hair_style = 7;
        owner.create_draft.stats = [71, 72, 73, 64, 65, 66, 67, 68];
        owner.pending_create_name = Some("Harnesshero".into());
        owner.creation_sent = true;
        let result = dispatch_preworld_action(
            &mut owner,
            PreWorldAction::CustomizeToggleLock {
                field: CustomizerField::HairStyle,
            },
        );
        assert!(result.refused.is_none());
        let before = format!("{owner:?}");

        let observed = project_character_snapshot(
            &owner.create_draft,
            owner.customizer,
            owner.overview.as_ref(),
            owner.selected_protocol_slot,
            owner.pending_create_name.as_deref(),
            owner.creation_sent,
        );

        assert_eq!(observed.draft.name, owner.create_draft.name);
        assert_eq!(observed.draft.face_type, owner.create_draft.face_type);
        assert_eq!(observed.draft.hair_style, owner.create_draft.hair_style);
        assert_eq!(observed.draft.stats, owner.create_draft.stats);
        assert_eq!(observed.pending_create_name, owner.pending_create_name);
        assert_eq!(observed.creation_sent, owner.creation_sent);
        assert!(matches!(
            observed.locks,
            Known::Known { ref value } if value == &["hair_style"]
        ));
        assert_eq!(format!("{owner:?}"), before, "snapshot mutated its owner");
    }
}
