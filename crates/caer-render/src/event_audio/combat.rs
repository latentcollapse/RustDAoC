//! CombatAnimation 0xBC → logical sound (System 8 stream Combat).

use caer_protocol::combat_anim::CombatResult;

/// Logical name for a combat result, or `None` when the event must stay silent.
///
/// HIT → impact bed (`Combat` → Combat1..3.wav). MISS / FUMBLE → no sound (anti-optimism:
/// a miss must not fire a hit bed). PARRY/BLOCK → shield impact; EVADE → whoosh.
#[must_use]
pub fn logical_for_combat_result(result: CombatResult) -> Option<&'static str> {
    match result {
        CombatResult::HitUnstyled | CombatResult::HitStyle => Some("Combat"),
        CombatResult::Parried | CombatResult::Blocked => Some("Shield_ImpactLight"),
        CombatResult::Evaded => Some("EvadeWhoosh"),
        CombatResult::Missed | CombatResult::Fumbled => None,
        CombatResult::Other(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caer_assets::soundmap::resolve_logical_wavs;
    use std::path::PathBuf;

    #[test]
    fn hit_maps_to_combat_bed_miss_is_silent() {
        assert_eq!(
            logical_for_combat_result(CombatResult::HitUnstyled),
            Some("Combat")
        );
        assert_eq!(
            logical_for_combat_result(CombatResult::HitStyle),
            Some("Combat")
        );
        assert_eq!(logical_for_combat_result(CombatResult::Missed), None);
        assert_eq!(logical_for_combat_result(CombatResult::Fumbled), None);
    }

    #[test]
    fn miss_must_not_share_hit_logical_name() {
        let hit = logical_for_combat_result(CombatResult::HitUnstyled).expect("hit");
        assert_ne!(
            logical_for_combat_result(CombatResult::Missed),
            Some(hit),
            "MISS must not fire the HIT sound"
        );
        assert_ne!(
            logical_for_combat_result(CombatResult::Parried),
            Some(hit),
            "PARRY must not fire the HIT sound"
        );
        assert_ne!(logical_for_combat_result(CombatResult::Blocked), Some(hit));
        assert_ne!(logical_for_combat_result(CombatResult::Evaded), Some(hit));
        assert_ne!(logical_for_combat_result(CombatResult::Fumbled), Some(hit));
        assert_ne!(
            logical_for_combat_result(CombatResult::Other(0x14)),
            Some(hit),
            "unknown result byte must not share the HIT bed"
        );
        assert_ne!(
            logical_for_combat_result(CombatResult::HitStyle).expect("style hit"),
            logical_for_combat_result(CombatResult::Missed).unwrap_or(""),
        );
    }

    /// End-to-end instrument: HIT records `Combat`; MISS records nothing (no device needed).
    #[test]
    fn play_logged_hit_records_miss_stays_silent() {
        use crate::audio::{play_logged, AudioCallLog};
        let mut audio = None;
        let mut log = AudioCallLog::new();
        if let Some(name) = logical_for_combat_result(CombatResult::HitUnstyled) {
            play_logged(&mut audio, &mut log, name);
        }
        if let Some(name) = logical_for_combat_result(CombatResult::Missed) {
            play_logged(&mut audio, &mut log, name);
        }
        assert!(log.contains("Combat"));
        assert_eq!(log.calls(), ["Combat"]);
    }

    #[test]
    fn mapped_combat_names_resolve_when_client_present() {
        let Some(root) = std::env::var_os("CAER_CLIENT") else {
            eprintln!("skip: CAER_CLIENT unset");
            return;
        };
        let root = PathBuf::from(root);
        for r in [
            CombatResult::HitUnstyled,
            CombatResult::Parried,
            CombatResult::Blocked,
            CombatResult::Evaded,
        ] {
            let name = logical_for_combat_result(r).expect("mapped");
            assert!(
                resolve_logical_wavs(&root, name).is_some(),
                "{name} must resolve under CAER_CLIENT/sounds for {r:?}"
            );
        }
    }
}
