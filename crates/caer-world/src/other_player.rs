//! Typed other-player identity owner: PlayerCreate → race/gender or unresolved diagnostic.
//!
//! One owner for the protocol → [`crate::WorldState`] → renderer hook. Unknown living-model
//! shorts stay [`OtherPlayerAvatar::Unresolved`]; they are never coerced to Highlander Female
//! (or any other default body). INT consumes [`OtherPlayerAvatar::from_player_create`] and
//! [`crate::WorldState::apply_player_create`] / [`crate::WorldState::unresolved_living_model_count`].

use caer_protocol::{
    customization::AvatarAppearance,
    entities::{player_living_model_id, race_gender_from_living_model, Player},
};

/// Authoritative other-player body identity derived from PlayerCreate living-model bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtherPlayerAvatar {
    /// OPEN_ORACLE `eLivingModel` hit — fig3 `(eRace, gender 1/2)`.
    Resolved {
        race: u8,
        gender: u8,
        living_model: u16,
        /// The complete source-authored look this player actually wears. Carried with the
        /// identity rather than looked up later: the living model says WHICH body, and this says
        /// WHOSE face, hair, and facial-morph geometry.
        appearance: AvatarAppearance,
    },
    /// Low-11 bits did not match `eLivingModel`. Diagnostic box + counter; never a guessed mesh.
    Unresolved { living_model: u16 },
}

impl OtherPlayerAvatar {
    /// Apply-from-PlayerCreate: the only legal construction path for other-player identity.
    #[must_use]
    pub fn from_player_create(player: &Player) -> Self {
        let living_model = player_living_model_id(player.model_unverified);
        match race_gender_from_living_model(living_model) {
            Some((race, gender)) => Self::Resolved {
                race,
                gender,
                living_model,
                appearance: player.appearance(),
            },
            None => Self::Unresolved { living_model },
        }
    }

    #[must_use]
    pub fn race_gender(self) -> Option<(u8, u8)> {
        match self {
            Self::Resolved { race, gender, .. } => Some((race, gender)),
            Self::Unresolved { .. } => None,
        }
    }

    /// The look to assemble this body with. An unresolved player draws no body at all, so it has
    /// no look to give.
    #[must_use]
    pub fn customization(self) -> caer_protocol::customization::Customization {
        match self {
            Self::Resolved { appearance, .. } => appearance.customization,
            Self::Unresolved { .. } => caer_protocol::customization::Customization::default(),
        }
    }

    /// The full look to assemble this body with. An unresolved player draws no body at all, so
    /// it has no appearance to give.
    #[must_use]
    pub fn appearance(self) -> AvatarAppearance {
        match self {
            Self::Resolved { appearance, .. } => appearance,
            Self::Unresolved { .. } => AvatarAppearance::default(),
        }
    }

    #[must_use]
    pub fn living_model(self) -> u16 {
        match self {
            Self::Resolved { living_model, .. } | Self::Unresolved { living_model } => living_model,
        }
    }

    #[must_use]
    pub fn is_unresolved(self) -> bool {
        matches!(self, Self::Unresolved { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Kind, WorldState};
    use caer_protocol::death::{PlayerDeath, PlayerRevive};
    use caer_protocol::entities::EntityUpdate;
    use caer_protocol::equipment::{slot, EquipmentUpdate, VisibleItem};
    use caer_protocol::session::ServerEvent;

    fn player(oid: u16, model_unverified: u16, name: &str) -> Player {
        Player {
            object_id: oid,
            session_id: oid,
            x: 561_400.0,
            y: 511_410.0,
            z: 2329.0,
            heading: 100,
            model_unverified,
            level: 50,
            realm: 1,
            flags: 0x04,
            name: name.into(),
            guild: String::new(),
            last_name: String::new(),
            custom: caer_protocol::customization::Customization::default(),
            eye_size: 0,
            lip_size: 0,
        }
    }

    /// Named falsifier: `unknown_living_model_unresolved_fallback_not_highlander_female`.
    #[test]
    fn unknown_living_model_unresolved_fallback_not_highlander_female() {
        // Captured Cotswold Feile/Lilillyn low-11 values are not eLivingModel.
        let feile = OtherPlayerAvatar::from_player_create(&player(16745, 0x9102, "Feile"));
        assert_eq!(feile, OtherPlayerAvatar::Unresolved { living_model: 258 });
        assert_ne!(
            feile.race_gender(),
            Some((3, 2)),
            "must not default Highlander Female"
        );
        assert!(feile.is_unresolved());

        let known = OtherPlayerAvatar::from_player_create(&player(99, 0x8000 | 43, "Known"));
        assert_eq!(
            known,
            OtherPlayerAvatar::Resolved {
                race: 3,
                gender: 2,
                living_model: 43,
                appearance: AvatarAppearance::default(),
            }
        );
    }

    /// Two simultaneous distinct remotes; neither is Self_.
    #[test]
    fn two_simultaneous_distinct_player_identities_are_not_self() {
        let mut w = WorldState::new();
        w.apply(&ServerEvent::PlayerPosition {
            x: 10.0,
            y: 10.0,
            z: 10.0,
            object_id: 7,
            heading: 0,
        });
        w.apply_player_create(&player(101, 32, "BritonM")); // BritonMale
        w.apply_player_create(&player(102, 503, "NorseM")); // NorseMale

        assert_eq!(w.get(7).unwrap().kind, Kind::Self_);
        assert!(
            w.other_player_avatar(7).is_none(),
            "Self_ is not an other-player"
        );
        let a = w.other_player_avatar(101).expect("briton");
        let b = w.other_player_avatar(102).expect("norse");
        assert_eq!(a.race_gender(), Some((1, 1)));
        assert_eq!(b.race_gender(), Some((5, 1)));
        assert_ne!(a, b);
        assert_eq!(w.unresolved_living_model_count(), 0);
        assert!(w.pick_other_player([561_400, 511_410], 50_000).is_some());
        // Self_ must not win other-player pick at its own seat.
        assert_ne!(w.pick_other_player([10, 10], 5), Some(7));
    }

    #[test]
    fn create_update_equipment_death_revive_pick_removal() {
        let mut w = WorldState::new();
        w.apply_player_create(&player(101, 32, "BritonM"));
        w.apply_player_create(&player(102, 503, "NorseM"));
        assert_eq!(w.get(101).unwrap().pos, [561_400, 511_410, 2329]);

        w.apply(&ServerEvent::EntityUpdated(EntityUpdate {
            object_id: 101,
            speed: 120,
            heading: 0x0400,
            local_x: 13000,
            local_y: 27000,
            z: 2445,
            target_id: 102,
            health_pct: 80,
            flags: 0,
            zone: 0,
        }));
        let e = w.get(101).unwrap();
        assert_eq!(e.speed, 120);
        assert_eq!(e.heading, 0x0400);
        assert_eq!(e.target_id, 102);
        assert_eq!(e.health_pct, 80);
        assert_ne!(e.pos, [561_400, 511_410, 2329]);
        let moved = e.pos;

        w.apply(&ServerEvent::EquipmentUpdated(EquipmentUpdate {
            object_id: 101,
            items: vec![VisibleItem {
                slot: slot::TORSO,
                model: 401,
                extension: Some(4),
                ..Default::default()
            }],
            ..Default::default()
        }));
        assert!(w.equipment_of(101).is_some());
        assert!(w.equipment_of(102).is_none());

        w.apply(&ServerEvent::PlayerDied(PlayerDeath {
            object_id: 101,
            killer_id: 102,
        }));
        assert!(w.is_dead(101));
        assert!(!w.is_dead(102));
        w.apply(&ServerEvent::PlayerRevived(PlayerRevive { object_id: 101 }));
        assert!(!w.is_dead(101));

        assert_eq!(w.pick_other_player([moved[0], moved[1]], 100), Some(101));

        w.apply(&ServerEvent::ObjectRemoved { object_id: 101 });
        assert!(w.get(101).is_none());
        assert!(w.other_player_avatar(101).is_none());
        assert!(w.equipment_of(101).is_none());
        assert_eq!(
            w.get(102).unwrap().target_id,
            0,
            "stale pick/target must clear"
        );
        assert_ne!(w.pick_other_player([moved[0], moved[1]], 100), Some(101));
    }

    #[test]
    fn unresolved_counter_tracks_unknown_and_drops_on_remove() {
        let mut w = WorldState::new();
        w.apply_player_create(&player(1, 0x9102, "Feile"));
        w.apply_player_create(&player(2, 32, "BritonM"));
        assert_eq!(w.unresolved_living_model_count(), 1);
        assert_eq!(w.unresolved_living_model_ids(), vec![1]);
        w.apply(&ServerEvent::ObjectRemoved { object_id: 1 });
        assert_eq!(w.unresolved_living_model_count(), 0);
    }
}
