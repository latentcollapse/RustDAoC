//! Footstep audio from movement + terrain material (System 8 stream Footstep).

/// Coarse surface class derived from a `textures.csv` base texture stem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceKind {
    Dirt,
    Grass,
    Stone,
    Sand,
    Snow,
    Mud,
    Wood,
    Default,
}

/// Classify a TerrainTex / textures.csv base name into a footstep surface.
///
/// Uses substring tokens from the client's own material names (Stream C atmosphere tables) —
/// never invents a texture, only maps an existing base string.
#[must_use]
pub fn classify_material_base(base: &str) -> SurfaceKind {
    let b = base.to_ascii_lowercase();
    // Order matters: more specific tokens first (cobble/stone before generic dirt).
    if b.contains("snow") {
        SurfaceKind::Snow
    } else if b.contains("sand") || b.contains("sandy") {
        SurfaceKind::Sand
    } else if b.contains("mud") || b.contains("clay") {
        SurfaceKind::Mud
    } else if b.contains("wood") {
        SurfaceKind::Wood
    } else if b.contains("stone")
        || b.contains("cobble")
        || b.contains("rock")
        || b.contains("boulder")
    {
        SurfaceKind::Stone
    } else if b.contains("grass") || b.contains("moss") {
        SurfaceKind::Grass
    } else if b.contains("dirt") {
        SurfaceKind::Dirt
    } else {
        SurfaceKind::Default
    }
}

/// Logical footstep name for a surface. Stone uses the on-stone bed; everything else uses `step`
/// (resolves to step5..9 under the client's sounds/). Different materials must not all collapse
/// to one hardcoded path when a discriminating stone bed exists.
#[must_use]
pub fn logical_for_footstep(kind: SurfaceKind) -> &'static str {
    match kind {
        SurfaceKind::Stone => "steps_onstone(d)",
        SurfaceKind::Dirt
        | SurfaceKind::Grass
        | SurfaceKind::Sand
        | SurfaceKind::Snow
        | SurfaceKind::Mud
        | SurfaceKind::Wood
        | SurfaceKind::Default => "step",
    }
}

/// Distance (world units) between footstep triggers while moving.
pub const FOOTSTEP_STRIDE: f32 = 90.0;

/// Whether stride accumulation should fire a footstep this frame.
///
/// Shared by the live move loop and the idle falsifier — hardcoded `moving = false` in a test
/// alone is not discriminating (REDTEAM-B).
#[must_use]
pub fn should_play_footstep(moving: bool, accum: f32) -> bool {
    moving && accum >= FOOTSTEP_STRIDE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{play_logged, AudioCallLog};
    use caer_assets::soundmap::resolve_logical_wavs;
    use std::path::PathBuf;

    #[test]
    fn stone_and_grass_map_to_different_logical_names() {
        let stone = classify_material_base("cobblestonepath");
        let grass = classify_material_base("darkgrass");
        assert_eq!(stone, SurfaceKind::Stone);
        assert_eq!(grass, SurfaceKind::Grass);
        assert_ne!(logical_for_footstep(stone), logical_for_footstep(grass));
    }

    #[test]
    fn standing_still_records_no_footstep() {
        // Idle must not fire even with a full stride of leftover accum.
        assert!(
            !should_play_footstep(false, FOOTSTEP_STRIDE * 2.0),
            "idle + large accum must not trigger"
        );
        assert!(
            should_play_footstep(true, FOOTSTEP_STRIDE),
            "moving at stride must trigger"
        );
        let mut audio = None;
        let mut log = AudioCallLog::new();
        if should_play_footstep(false, FOOTSTEP_STRIDE * 2.0) {
            play_logged(
                &mut audio,
                &mut log,
                logical_for_footstep(SurfaceKind::Default),
            );
        }
        assert!(log.is_empty(), "idle must not fire footstep");
    }

    #[test]
    fn footstep_names_resolve_when_client_present() {
        let Some(root) = std::env::var_os("CAER_CLIENT") else {
            eprintln!("skip: CAER_CLIENT unset");
            return;
        };
        let root = PathBuf::from(root);
        for kind in [SurfaceKind::Default, SurfaceKind::Grass, SurfaceKind::Stone] {
            let name = logical_for_footstep(kind);
            assert!(
                resolve_logical_wavs(&root, name).is_some(),
                "{name} must resolve for {kind:?}"
            );
        }
    }
}
