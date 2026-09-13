//! INT hook: other-player identity protocol → `WorldState` → renderer.
//!
//! Does **not** wire `rustdaoc`. Product loop consumes:
//! - [`OtherPlayerAvatars::sync_from_world`]
//! - [`OtherPlayerAvatars::unresolved_living_model_count`]
//! - [`caer_world::OtherPlayerAvatar::from_player_create`] / [`caer_world::WorldState::apply_player_create`]
//!
//! Resolved identities request fig3 via [`crate::entities::EntityModels::ensure_avatar`].
//! Unresolved living models emit a diagnostic box colour and increment the counter — never a
//! default Highlander Female body.

use std::collections::HashSet;

use caer_world::{Kind, OtherPlayerAvatar, WorldState};

use crate::entities::EntityModels;

/// Visible diagnostic box colour for genuinely unresolved living-model identities.
/// Distinct from [`crate::color_for`] `Kind::Player` green so fallback cannot masquerade as coverage.
pub const UNRESOLVED_LIVING_MODEL_COLOR: [f32; 3] = [0.95, 0.45, 0.05];

/// How an other-player should be drawn this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtherPlayerDrawKind {
    /// Real fig3 body: race + fig3 gender (1 male / 2 female).
    Fig3 {
        race: u8,
        gender: u8,
        living_model: u16,
    },
    /// Unknown living-model short — diagnostic box, never a guessed mesh.
    UnresolvedDiagnostic { living_model: u16 },
}

/// One other-player row in the INT draw plan (PlayerCreate-derived; not Self_).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OtherPlayerDraw {
    pub object_id: u16,
    pub kind: OtherPlayerDrawKind,
    pub pos: [i32; 3],
    pub heading: u16,
    pub speed: u16,
    pub is_dead: bool,
    pub target_id: u16,
}

/// Typed renderer-side owner: animation claim + unresolved diagnostic set.
///
/// Identity itself lives on [`WorldState`] (`OtherPlayerAvatar`). This type is the INT-facing
/// hook that syncs that identity into renderer ownership (anim release on ObjectRemoved).
#[derive(Debug, Default, Clone)]
pub struct OtherPlayerAvatars {
    unresolved_ids: HashSet<u16>,
    /// Object ids whose locomotion state this owner currently claims.
    anim_owned: HashSet<u16>,
}

impl OtherPlayerAvatars {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild unresolved + anim-claim sets from WorldState (PlayerCreate-derived).
    ///
    /// Releases animation on object ids that left the world (ObjectRemoved / region cull).
    /// When `em` is provided, also drops [`EntityModels`] anim state immediately.
    pub fn sync_from_world(&mut self, world: &WorldState, mut em: Option<&mut EntityModels>) {
        let mut live = HashSet::new();
        let mut unresolved = HashSet::new();
        for v in world.iter() {
            if v.kind != Kind::Player {
                continue;
            }
            live.insert(v.object_id);
            if world
                .other_player_avatar(v.object_id)
                .is_some_and(OtherPlayerAvatar::is_unresolved)
            {
                unresolved.insert(v.object_id);
            }
        }
        let dropped: Vec<u16> = self
            .anim_owned
            .iter()
            .copied()
            .filter(|id| !live.contains(id))
            .collect();
        for id in dropped {
            self.release_anim(id, em.as_deref_mut());
        }
        self.unresolved_ids = unresolved;
        // Keep claims only for remotes still in world; unresolved never own a rig.
        self.anim_owned
            .retain(|id| live.contains(id) && !self.unresolved_ids.contains(id));
    }

    /// Record that `render_world` / INT ticked locomotion for this remote.
    pub fn take_anim(&mut self, object_id: u16) {
        self.anim_owned.insert(object_id);
        self.unresolved_ids.remove(&object_id);
    }

    #[must_use]
    pub fn has_anim(&self, object_id: u16) -> bool {
        self.anim_owned.contains(&object_id)
    }

    /// ObjectRemoved path: drop body claim, diagnostic id, and animation ownership.
    pub fn on_object_removed(&mut self, object_id: u16, em: Option<&mut EntityModels>) -> bool {
        self.unresolved_ids.remove(&object_id);
        self.release_anim(object_id, em)
    }

    fn release_anim(&mut self, object_id: u16, em: Option<&mut EntityModels>) -> bool {
        let ours = self.anim_owned.remove(&object_id);
        if let Some(em) = em {
            em.release_anim(object_id);
        }
        ours
    }

    #[must_use]
    pub fn unresolved_living_model_count(&self) -> usize {
        self.unresolved_ids.len()
    }

    #[must_use]
    pub fn unresolved_ids(&self) -> Vec<u16> {
        let mut ids: Vec<u16> = self.unresolved_ids.iter().copied().collect();
        ids.sort_unstable();
        ids
    }
}

/// PlayerCreate-derived draw plan. Direct `ensure_avatar(race, gender)` is component-only;
/// product proof starts here.
#[must_use]
pub fn draw_plan(world: &WorldState) -> Vec<OtherPlayerDraw> {
    let mut out: Vec<OtherPlayerDraw> = world
        .iter()
        .filter(|v| v.kind == Kind::Player)
        .filter_map(|v| {
            let avatar = world.other_player_avatar(v.object_id)?;
            let kind = match avatar {
                OtherPlayerAvatar::Resolved {
                    race,
                    gender,
                    living_model,
                    ..
                } => OtherPlayerDrawKind::Fig3 {
                    race,
                    gender,
                    living_model,
                },
                OtherPlayerAvatar::Unresolved { living_model } => {
                    OtherPlayerDrawKind::UnresolvedDiagnostic { living_model }
                }
            };
            Some(OtherPlayerDraw {
                object_id: v.object_id,
                kind,
                pos: v.pos,
                heading: v.heading,
                speed: v.speed,
                is_dead: v.is_dead,
                target_id: v.target_id,
            })
        })
        .collect();
    out.sort_by_key(|d| d.object_id);
    out
}

/// Living other-players that the mesh/box path would emit (dead culled; Unknown excluded).
#[must_use]
pub fn living_other_player_ids(world: &WorldState) -> Vec<u16> {
    let mut ids: Vec<u16> = draw_plan(world)
        .into_iter()
        .filter(|d| crate::death::in_living_draw(d.is_dead))
        .map(|d| d.object_id)
        .collect();
    ids.sort_unstable();
    ids
}

/// Box colour for an other-player with no mesh this frame.
#[must_use]
pub fn box_color(avatar: Option<OtherPlayerAvatar>, lod: u8) -> [f32; 3] {
    match avatar {
        Some(OtherPlayerAvatar::Unresolved { .. }) => UNRESOLVED_LIVING_MODEL_COLOR,
        _ => crate::color_for(Kind::Player, lod),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caer_protocol::entities::Player;
    use caer_protocol::session::ServerEvent;
    use caer_world::WorldState;

    fn player(oid: u16, model: u16, name: &str) -> Player {
        Player {
            object_id: oid,
            session_id: oid,
            x: 100.0,
            y: 200.0,
            z: 10.0,
            heading: 0,
            model_unverified: model,
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

    #[test]
    fn draw_plan_from_player_create_two_identities_and_unresolved_diagnostic() {
        let mut w = WorldState::new();
        w.apply(&ServerEvent::PlayerPosition {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            object_id: 1,
            heading: 0,
        });
        w.apply_player_create(&player(10, 32, "BritonM"));
        w.apply_player_create(&player(11, 503, "NorseM"));
        w.apply_player_create(&player(12, 0x9102, "Feile"));

        let plan = draw_plan(&w);
        assert_eq!(plan.len(), 3, "Self_ must not appear as an other-player");
        assert!(plan.iter().all(|d| d.object_id != 1));
        match plan[0].kind {
            OtherPlayerDrawKind::Fig3 { race, gender, .. } => {
                assert_eq!((race, gender), (1, 1));
            }
            other => panic!("expected BritonMale fig3, got {other:?}"),
        }
        match plan[1].kind {
            OtherPlayerDrawKind::Fig3 { race, gender, .. } => {
                assert_eq!((race, gender), (5, 1));
            }
            other => panic!("expected NorseMale fig3, got {other:?}"),
        }
        match plan[2].kind {
            OtherPlayerDrawKind::UnresolvedDiagnostic { living_model } => {
                assert_eq!(living_model, 258);
            }
            OtherPlayerDrawKind::Fig3 { race, gender, .. } => {
                panic!(
                    "unknown short must not become fig3 ({race},{gender}) — not Highlander Female"
                );
            }
        }
        assert_ne!(
            box_color(w.other_player_avatar(12), 0),
            crate::color_for(Kind::Player, 0),
            "unresolved diagnostic must not match coverage-green player boxes"
        );

        let mut owner = OtherPlayerAvatars::new();
        owner.sync_from_world(&w, None);
        assert_eq!(owner.unresolved_living_model_count(), 1);
        owner.take_anim(10);
        owner.take_anim(11);
        assert!(owner.has_anim(10));
        w.apply(&ServerEvent::ObjectRemoved { object_id: 10 });
        assert!(owner.on_object_removed(10, None));
        assert!(!owner.has_anim(10));
        owner.sync_from_world(&w, None);
        assert!(!owner.has_anim(10));
        assert_eq!(owner.unresolved_living_model_count(), 1);
    }

    #[test]
    fn death_culls_living_other_player_ids() {
        use caer_protocol::death::PlayerDeath;
        let mut w = WorldState::new();
        w.apply_player_create(&player(10, 32, "BritonM"));
        assert_eq!(living_other_player_ids(&w), vec![10]);
        w.apply(&ServerEvent::PlayerDied(PlayerDeath {
            object_id: 10,
            killer_id: 0,
        }));
        assert!(living_other_player_ids(&w).is_empty());
    }
}
