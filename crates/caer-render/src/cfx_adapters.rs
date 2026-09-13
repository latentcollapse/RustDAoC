//! Stock-skin adapter *types* for combat / cast / status (CFX → UI lane).
//!
//! These resolve Atlantis-authored names from typed CFX state. They do **not** claim the skin
//! chrome is drawn here — INT/UI wires them into [`crate::adapters::AdapterState`] later.
//! rustdaoc is not modified by this lane.

use caer_world::{CastPresentation, CombatPresentation, ConBand};

/// Adapter names the CFX lane owns. Unknown names stay unbound (empty label, not filler text).
#[must_use]
pub fn resolve(presentation: &CombatPresentation<'_>, adapter: &str) -> Option<String> {
    match adapter {
        "summary_cast_spell" => {
            let bar = presentation.cast.self_bar?;
            Some(format!("{}", bar.spell_id))
        }
        "summary_cast_time" => {
            let bar = presentation.cast.self_bar?;
            Some(format!("{:.1}s", f32::from(bar.cast_time) / 10.0))
        }
        "cast_state" => Some(cast_state_label(&presentation.cast).to_string()),
        "xp_percent" => {
            let p = presentation.points?;
            Some(format!("{}", p.level_permill / 10))
        }
        "bounty_points" => Some(presentation.points?.bounty_points.to_string()),
        "realm_points" => Some(presentation.points?.realm_points.to_string()),
        "effect_icon0" => presentation
            .icons
            .first()
            .map(|e| format!("{}:{}", e.icon, e.name)),
        "concentration_effect0" => presentation
            .concentration
            .first()
            .map(|e| format!("{}:{}", e.icon, e.name)),
        "combat_result" => presentation
            .combat_results
            .last()
            .map(|r| r.result_label.to_string()),
        "combat_result_hp" => presentation
            .combat_results
            .last()
            .map(|r| format!("{}%", r.target_health_pct)),
        "pet_oid" => presentation.pet.map(|p| p.pet_id.to_string()),
        _ => None,
    }
}

#[must_use]
pub fn cast_state_label(cast: &CastPresentation) -> &'static str {
    if cast.self_bar.is_some() {
        "casting"
    } else {
        match cast.outcome {
            Some(caer_world::CastOutcome::Completed { .. }) => "completed",
            Some(caer_world::CastOutcome::Failed { .. }) => "failed",
            Some(caer_world::CastOutcome::Interrupted { .. }) => "interrupted",
            None => "idle",
        }
    }
}

/// Target con adapter — requires typed levels, never chat.
#[must_use]
pub fn resolve_con(band: Option<ConBand>, adapter: &str) -> Option<String> {
    match adapter {
        "summary_target_con" => Some(band?.label().to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caer_protocol::session::ServerEvent;
    use caer_world::WorldState;

    #[test]
    fn cast_adapters_require_self_bar_not_peer() {
        let mut w = WorldState::new();
        w.apply(&ServerEvent::PlayerPosition {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            object_id: 1,
            heading: 0,
        });
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 9,
                spell_id: 3011,
                cast_time: 25,
            },
        ));
        let p = w.combat_presentation();
        assert_eq!(resolve(&p, "summary_cast_spell"), None);
        assert_eq!(resolve(&p, "cast_state").as_deref(), Some("idle"));
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 1,
                spell_id: 3011,
                cast_time: 25,
            },
        ));
        let p = w.combat_presentation();
        assert_eq!(resolve(&p, "summary_cast_spell").as_deref(), Some("3011"));
        assert_eq!(resolve(&p, "cast_state").as_deref(), Some("casting"));
    }

    #[test]
    fn interrupt_adapter_is_not_completed() {
        let mut w = WorldState::new();
        w.apply(&ServerEvent::PlayerPosition {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            object_id: 1,
            heading: 0,
        });
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 1,
                spell_id: 2,
                cast_time: 10,
            },
        ));
        w.apply(&ServerEvent::SpellInterrupted(
            caer_protocol::spells::InterruptSpellCast { object_id: 1 },
        ));
        let p = w.combat_presentation();
        assert_eq!(resolve(&p, "cast_state").as_deref(), Some("interrupted"));
        assert_eq!(resolve(&p, "summary_cast_spell"), None);
    }

    #[test]
    fn xp_and_bounty_require_points_packet() {
        let w = WorldState::new();
        let p = w.combat_presentation();
        assert_eq!(resolve(&p, "bounty_points"), None);
        assert_eq!(resolve(&p, "xp_percent"), None);
        let mut w = WorldState::new();
        w.apply(&ServerEvent::CharacterPoints(
            caer_protocol::points::CharacterPoints {
                realm_points: 5,
                level_permill: 420,
                skill_specialty_points: 0,
                bounty_points: 12,
                realm_specialty_points: 0,
                champion_level_permill: 0,
                experience: None,
                experience_for_next_level: None,
            },
        ));
        let p = w.combat_presentation();
        assert_eq!(resolve(&p, "bounty_points").as_deref(), Some("12"));
        assert_eq!(resolve(&p, "xp_percent").as_deref(), Some("42"));
    }

    #[test]
    fn combat_result_adapter_ignores_empty_control() {
        let w = WorldState::new();
        assert_eq!(resolve(&w.combat_presentation(), "combat_result"), None);
    }

    #[test]
    fn con_adapter_unbound_without_levels() {
        assert_eq!(resolve_con(None, "summary_target_con"), None);
        assert_eq!(
            resolve_con(Some(ConBand::Red), "summary_target_con").as_deref(),
            Some("red")
        );
    }

    #[test]
    fn pet_adapter_requires_pet_window_packet() {
        let w = WorldState::new();
        assert_eq!(resolve(&w.combat_presentation(), "pet_oid"), None);
        let mut w = WorldState::new();
        w.apply(&ServerEvent::PetWindow(caer_protocol::pets::PetWindow {
            pet_id: 88,
            action: caer_protocol::pets::PetWindowAction::Open,
            aggro: caer_protocol::pets::PetAggro::Passive,
            walk: caer_protocol::pets::PetWalk::Follow,
            icons: vec![],
        }));
        assert_eq!(
            resolve(&w.combat_presentation(), "pet_oid").as_deref(),
            Some("88")
        );
    }
}
