//! SpellCast 0x72 / SpellEffect 0x1B → logical sound (System 8 stream Spell).

use caer_protocol::spells::SpellEffectAnimation;

/// Cast-start bed — fires on SpellCastAnimation 0x72.
pub const SPELL_CAST_LOGICAL: &str = "SpellMagicMissle_Cast";

/// Effect-land bed — fires on successful SpellEffectAnimation 0x1B with sound enabled.
pub const SPELL_EFFECT_LOGICAL: &str = "SpellMagicMissle_Hit";

#[must_use]
pub fn logical_for_spell_cast() -> &'static str {
    SPELL_CAST_LOGICAL
}

/// Map a world-owned [`caer_world::SoundRequest`] to a logical name. EffectLand is absent on
/// interrupt/fail because WorldState never enqueues it.
#[must_use]
pub fn logical_for_sound_request(req: caer_world::SoundRequest) -> &'static str {
    match req.kind {
        caer_world::SoundKind::CastWindup => SPELL_CAST_LOGICAL,
        caer_world::SoundKind::EffectLand => SPELL_EFFECT_LOGICAL,
    }
}

/// Effect land sound, or `None` when the packet says mute / failed (refuse / no_sound).
#[must_use]
pub fn logical_for_spell_effect(effect: &SpellEffectAnimation) -> Option<&'static str> {
    if effect.no_sound || effect.success == 0 {
        None
    } else {
        Some(SPELL_EFFECT_LOGICAL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{play_logged, AudioCallLog};
    use caer_assets::soundmap::resolve_logical_wavs;
    use caer_protocol::spells::{SpellCastAnimation, SpellEffectAnimation};
    use std::path::PathBuf;

    fn sample_effect(success: u8, no_sound: bool) -> SpellEffectAnimation {
        SpellEffectAnimation {
            caster_id: 1,
            spell_id: 407,
            target_id: 2,
            bolt_time: 0,
            no_sound,
            success,
        }
    }

    #[test]
    fn cast_and_effect_map_to_distinct_logical_names() {
        assert_eq!(logical_for_spell_cast(), SPELL_CAST_LOGICAL);
        assert_eq!(
            logical_for_spell_effect(&sample_effect(1, false)),
            Some(SPELL_EFFECT_LOGICAL)
        );
        assert_ne!(SPELL_CAST_LOGICAL, SPELL_EFFECT_LOGICAL);
    }

    #[test]
    fn refuse_or_mute_effect_stays_silent() {
        assert_eq!(logical_for_spell_effect(&sample_effect(0, false)), None);
        assert_eq!(logical_for_spell_effect(&sample_effect(1, true)), None);
        assert_eq!(logical_for_spell_effect(&sample_effect(0, true)), None);
    }

    #[test]
    fn play_logged_records_cast_and_skips_refused_effect() {
        let mut audio = None;
        let mut log = AudioCallLog::new();
        play_logged(&mut audio, &mut log, logical_for_spell_cast());
        // Refuse arm: no success → no effect sound even if we "cast".
        if let Some(name) = logical_for_spell_effect(&sample_effect(0, false)) {
            play_logged(&mut audio, &mut log, name);
        }
        assert_eq!(log.calls(), [SPELL_CAST_LOGICAL]);
        assert!(!log.contains(SPELL_EFFECT_LOGICAL));
    }

    #[test]
    fn mapped_spell_names_resolve_when_client_present() {
        let Some(root) = std::env::var_os("CAER_CLIENT") else {
            eprintln!("skip: CAER_CLIENT unset");
            return;
        };
        let root = PathBuf::from(root);
        for name in [SPELL_CAST_LOGICAL, SPELL_EFFECT_LOGICAL] {
            assert!(
                resolve_logical_wavs(&root, name).is_some(),
                "{name} must resolve under CAER_CLIENT/sounds"
            );
        }
        let _ = SpellCastAnimation {
            caster_id: 1,
            spell_id: 407,
            cast_time: 30,
        };
    }
}
