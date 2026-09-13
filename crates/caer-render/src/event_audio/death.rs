//! PlayerDeath 0xAE / PlayerRevive 0x89 → logical sound.
//!
//! Death uses the shipped humanoid male die bed (`avmdie.wav`) as the typed hook. Race/gender
//! die-table completeness is **not** claimed (mix parity gap).
//! Revive has no verified logical→file mapping in this leaf; the helper returns `None` rather
//! than inventing a name. Callers that pass an unmapped string through [`crate::audio_bus`] get
//! [`crate::audio_bus::PlayOutcome::Unmapped`], not success.

pub const PLAYER_DEATH_LOGICAL: &str = "avmdie";

#[must_use]
pub fn logical_for_player_death() -> &'static str {
    PLAYER_DEATH_LOGICAL
}

/// No verified revive bed. Silence is correct; do not invent `g_PlayerRevive`.
#[must_use]
pub fn logical_for_player_revive() -> Option<&'static str> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_bus::{AudioBus, LogicalSoundEvent, PlayOutcome, SoundCategory};
    use caer_assets::soundmap::resolve_logical_wavs;
    use std::path::PathBuf;

    #[test]
    fn revive_helper_is_silent_not_invented() {
        assert!(logical_for_player_revive().is_none());
    }

    #[test]
    fn death_name_resolves_when_client_present() {
        let Some(root) = std::env::var_os("CAER_CLIENT") else {
            eprintln!("skip: CAER_CLIENT unset");
            return;
        };
        let root = PathBuf::from(root);
        assert!(
            resolve_logical_wavs(&root, logical_for_player_death()).is_some(),
            "avmdie must resolve under CAER_CLIENT/sounds"
        );
        let mut bus = AudioBus::null(&root);
        assert_eq!(
            bus.play(LogicalSoundEvent::oneshot(
                SoundCategory::DeathRevive,
                logical_for_player_death()
            )),
            PlayOutcome::Played
        );
        assert_eq!(
            bus.play(LogicalSoundEvent::oneshot(
                SoundCategory::DeathRevive,
                "g_PlayerRevive"
            )),
            PlayOutcome::Unmapped
        );
    }
}
