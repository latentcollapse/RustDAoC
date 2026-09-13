//! Client-side request/target cache invalidation (S1 lifecycle).
//!
//! [`caer_world::WorldState`] already culls entities on `ObjectRemoved` / `RegionChanged` /
//! `LoggedOut`, but the player client also keeps:
//! - `requested_npcs` — ids already asked for via RequestNpc (insert-only until cleared)
//! - HUD `target` — selected object id + nameplate
//!
//! If those survive remove / region / logout, recycled object ids leave stale magenta boxes and a
//! sticky target frame after the entity is gone.

use std::collections::HashSet;

/// Drop a departed id from the NPC-create request set; clear target when it matches.
///
/// Returns `true` when the current target was cleared (caller may mirror `Target(0)` to the server).
#[must_use]
pub fn on_object_removed(
    requested_npcs: &mut HashSet<u16>,
    target: &mut Option<(u16, String)>,
    object_id: u16,
) -> bool {
    requested_npcs.remove(&object_id);
    let hit = target.as_ref().is_some_and(|(id, _)| *id == object_id);
    if hit {
        *target = None;
    }
    hit
}

/// RegionChanged / begin_region_change / LoggedOut (and reconnect): scrub both caches.
pub fn on_region_or_logout(requested_npcs: &mut HashSet<u16>, target: &mut Option<(u16, String)>) {
    requested_npcs.clear();
    *target = None;
}

/// Reconnect is the same scrub as logout for HUD target / request bits.
pub fn on_reconnect(requested_npcs: &mut HashSet<u16>, target: &mut Option<(u16, String)>) {
    on_region_or_logout(requested_npcs, target);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Named falsifier: ObjectRemoved must drop the request bit and matching target.
    /// Delete the `remove` / target-clear in [`on_object_removed`] → this fails.
    #[test]
    fn object_removed_clears_request_and_matching_target() {
        let mut requested = HashSet::from([10u16, 20, 30]);
        let mut target = Some((20u16, "a boar".into()));
        assert!(
            on_object_removed(&mut requested, &mut target, 20),
            "matching target must clear"
        );
        assert!(
            !requested.contains(&20),
            "ObjectRemoved must drop id from requested_npcs — otherwise reuse skips RequestNpc"
        );
        assert!(requested.contains(&10) && requested.contains(&30));
        assert!(
            target.is_none(),
            "ObjectRemoved must clear matching target — otherwise HUD sticks on recycled id"
        );
    }

    /// Named falsifier: ObjectRemoved for a different id must not wipe an unrelated target.
    #[test]
    fn object_removed_preserves_unrelated_target() {
        let mut requested = HashSet::from([7u16]);
        let mut target = Some((9u16, "guard".into()));
        assert!(!on_object_removed(&mut requested, &mut target, 7));
        assert!(!requested.contains(&7));
        assert_eq!(target.as_ref().map(|(id, _)| *id), Some(9));
    }

    /// Named falsifier: RegionChanged / begin_region_change / LoggedOut wipe both caches.
    /// Delete the `.clear()` / target wipe in [`on_region_or_logout`] → this fails.
    #[test]
    fn region_or_logout_clears_request_and_target() {
        let mut requested = HashSet::from([1u16, 2, 3]);
        let mut target = Some((2u16, "merchant".into()));
        on_region_or_logout(&mut requested, &mut target);
        assert!(
            requested.is_empty(),
            "region/logout must clear requested_npcs — reuse after zone would skip RequestNpc"
        );
        assert!(
            target.is_none(),
            "region/logout must clear target — otherwise HUD survives zone/logout"
        );
    }

    #[test]
    fn reconnect_clears_request_and_target() {
        let mut requested = HashSet::from([4u16]);
        let mut target = Some((4u16, "x".into()));
        on_reconnect(&mut requested, &mut target);
        assert!(requested.is_empty());
        assert!(target.is_none());
    }
}
