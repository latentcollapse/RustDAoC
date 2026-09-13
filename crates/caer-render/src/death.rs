//! Living-draw policy for PlayerDeath 0xAE / PlayerRevive 0x89.
//!
//! WorldState already marks `is_dead` from those packets alone (never from 0 HP). This module is
//! the **render consumption** of that flag: the living mesh/box path must skip dead entities so a
//! death/revive is player-visible. Revive clears `is_dead` and the same path draws them again.
//!
//! Binding (Sol-shaped): delete the `is_dead` gate → dead bodies keep drawing as living → this
//! module's falsifier fails. Inventing death from `health_pct == 0` alone is also a fail (REQ-020).

use caer_world::{Kind, WorldState};

/// Whether an entity belongs in the living mesh / placeholder-box draw this frame.
///
/// Dead entities are culled from the living path (DAoC ghost mesh is a later fidelity slice;
/// absence of the living body is the binding player-visible change for this wire).
#[must_use]
pub fn in_living_draw(is_dead: bool) -> bool {
    !is_dead
}

/// Dead entities remain visible as corpses (not vanish). Ghost mesh is still a later slice.
#[must_use]
pub fn in_corpse_draw(is_dead: bool) -> bool {
    is_dead
}

/// Object ids drawn as corpses this frame.
#[must_use]
pub fn corpse_draw_ids(world: &WorldState) -> Vec<u16> {
    let mut ids: Vec<u16> = world
        .iter()
        .filter(|v| v.kind != Kind::Unknown && in_corpse_draw(v.is_dead))
        .map(|v| v.object_id)
        .collect();
    ids.sort_unstable();
    ids
}

/// Object ids that the living draw path would emit meshes/boxes for (Unknown excluded).
///
/// Used by the named falsifier and any scenario probe that must not require a GPU.
#[must_use]
pub fn living_draw_ids(world: &WorldState) -> Vec<u16> {
    let mut ids: Vec<u16> = world
        .iter()
        .filter(|v| v.kind != Kind::Unknown && in_living_draw(v.is_dead))
        .map(|v| v.object_id)
        .collect();
    ids.sort_unstable();
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use caer_protocol::combat_anim::{CombatAnimation, CombatResult};
    use caer_protocol::death::{PlayerDeath, PlayerRevive};
    use caer_protocol::entities::Npc;
    use caer_protocol::session::ServerEvent;
    use caer_world::WorldState;

    fn seed_victim(world: &mut WorldState, object_id: u16) {
        world.apply(&ServerEvent::NpcInView(Npc {
            object_id,
            speed: 100,
            heading: 0,
            x: 1000,
            y: 2000,
            z: 8000,
            model: 40,
            size: 50,
            level: 10,
            flags: 0,
            name: "victim".into(),
            guild: String::new(),
        }));
    }

    /// Named falsifier: `player_death_culls_living_draw_revive_restores`.
    ///
    /// Observation that would fail if broken: remove `in_living_draw` / the `is_dead` continue in
    /// `render_world_timed` (and this helper) → after `PlayerDied`, oid 42 remains in
    /// [`living_draw_ids`] and the first assert fails. Anti-optimism arm: 0 HP from CombatAnimation
    /// alone must **not** cull.
    #[test]
    fn player_death_culls_living_draw_revive_restores() {
        let mut world = WorldState::new();
        seed_victim(&mut world, 42);
        assert_eq!(
            living_draw_ids(&world),
            vec![42],
            "seed living must be in the living draw set"
        );
        assert!(!world.is_dead(42));

        world.apply(&ServerEvent::PlayerDied(PlayerDeath {
            object_id: 42,
            killer_id: 7,
        }));
        assert!(world.is_dead(42), "0xAE must set is_dead");
        assert!(
            living_draw_ids(&world).is_empty(),
            "after PlayerDeath, living draw must cull oid 42 (got {:?})",
            living_draw_ids(&world)
        );
        assert_eq!(
            corpse_draw_ids(&world),
            vec![42],
            "after PlayerDeath, corpse draw must keep oid 42 (vanish is a defect)"
        );

        world.apply(&ServerEvent::PlayerRevived(PlayerRevive { object_id: 42 }));
        assert!(!world.is_dead(42), "0x89 must clear is_dead");
        assert_eq!(
            living_draw_ids(&world),
            vec![42],
            "after PlayerRevive, living draw must restore oid 42"
        );
        assert!(corpse_draw_ids(&world).is_empty());

        // REQ-020 anti-optimism: 0 HP without PlayerDeath is not death for render.
        let mut w = WorldState::new();
        seed_victim(&mut w, 99);
        w.apply(&ServerEvent::CombatAnimation(CombatAnimation {
            attacker_id: 1,
            defender_id: 99,
            weapon_id: 0,
            shield_id: 0,
            style: 0,
            stance: 0,
            result: CombatResult::HitUnstyled,
            target_health_pct: 0,
            unk: 0,
        }));
        assert_eq!(w.get(99).map(|e| e.health_pct), Some(0));
        assert!(!w.is_dead(99), "0 HP alone must not set is_dead");
        assert_eq!(
            living_draw_ids(&w),
            vec![99],
            "0 HP without 0xAE must still be in living draw"
        );
    }

    /// REDTEAM-B: bind the cull to real call sites. Deleting `in_living_draw` from
    /// `render_world_timed` / nameplates while leaving [`living_draw_ids`] alone must fail here.
    #[test]
    fn in_living_draw_is_wired_into_render_and_nameplates() {
        let render = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
        let plates = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/nameplates.rs"));
        assert!(
            render.contains("death::in_living_draw"),
            "falsifier: render_world_timed must gate living draw with death::in_living_draw"
        );
        assert!(
            plates.contains("death::in_living_draw")
                || plates.contains("crate::death::in_living_draw"),
            "falsifier: nameplates must gate with in_living_draw"
        );
    }
}
