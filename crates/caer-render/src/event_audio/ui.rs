//! Stock-skin UI → logical sound (cc_interface.csv: `s_click` / `s_rollover`).
//!
//! Provenance: `sounds/cc_interface.csv` names those beds; `CLICK.WAV` / `ROLLOVER.WAV` exist
//! under the client `sounds/` tree. Soundmap strip+file-exists is the resolution rule.

pub const UI_CLICK_LOGICAL: &str = "s_click";
pub const UI_ROLLOVER_LOGICAL: &str = "s_rollover";

#[must_use]
pub fn logical_for_ui_click() -> &'static str {
    UI_CLICK_LOGICAL
}

#[must_use]
pub fn logical_for_ui_rollover() -> &'static str {
    UI_ROLLOVER_LOGICAL
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_bus::{AudioBus, LogicalSoundEvent, PlayOutcome, SoundCategory};
    use caer_assets::soundmap::resolve_logical_wavs;
    use std::path::PathBuf;

    #[test]
    fn click_and_rollover_are_distinct() {
        assert_ne!(logical_for_ui_click(), logical_for_ui_rollover());
    }

    #[test]
    fn ui_names_resolve_when_client_present() {
        let Some(root) = std::env::var_os("CAER_CLIENT") else {
            eprintln!("skip: CAER_CLIENT unset");
            return;
        };
        let root = PathBuf::from(root);
        for name in [UI_CLICK_LOGICAL, UI_ROLLOVER_LOGICAL] {
            assert!(
                resolve_logical_wavs(&root, name).is_some(),
                "{name} must resolve under CAER_CLIENT/sounds"
            );
        }
        let mut bus = AudioBus::null(&root);
        let out = bus.play(LogicalSoundEvent::oneshot(
            SoundCategory::Ui,
            logical_for_ui_click(),
        ));
        assert_eq!(out, PlayOutcome::Played);
    }
}
