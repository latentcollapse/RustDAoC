//! Null-device CI surface for lane AUD (no physical audio hardware).
//!
//! Named falsifiers live with the unit tests in `audio_bus`; this file proves the public hook
//! is reachable as a crate integration test.

use caer_render::{AudioBus, LogicalSoundEvent, PlayOutcome, SoundCategory};

#[test]
fn public_bus_unmapped_is_not_success() {
    let root = std::env::temp_dir().join(format!(
        "caer-mb2-aud-it-{}-{}",
        std::process::id(),
        "unmapped"
    ));
    let sounds = root.join("sounds");
    std::fs::create_dir_all(&sounds).unwrap();
    std::fs::write(sounds.join("click.wav"), b"").unwrap();
    let mut bus = AudioBus::null(&root);
    let out = bus.play(LogicalSoundEvent::oneshot(
        SoundCategory::Ui,
        "g_PrairieWind",
    ));
    assert_eq!(out, PlayOutcome::Unmapped);
    assert!(!out.is_success());
    let _ = std::fs::remove_dir_all(&root);
}
