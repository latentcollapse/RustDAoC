//! Atmosphere from the client's own tables (CAER-1.0 leg 11 / MS-05).
//!
//! Feeds lighting, terrain shade, splat material catalog, and the global ocean plane from
//! authored data under `CAER_CLIENT` — not from the old shader hardcodes.
//!
//! **Falsifier:** set `CAER_ATMOSPHERE_TABLES=0` (or build without reading via
//! [`tables_enabled`]). Table reads are skipped and lighting/ocean/materials fall back to
//! *empty* — not the deleted hardcodes. If the frame still matched the table-fed look with
//! tables disabled, the tables would not be load-bearing.

mod lights;
mod materials;
mod ocean;
mod shademap;
mod sky_light;

pub use lights::{lights_from_mpk, parse_lights_csv, to_world, ZoneLight, ZoneLights};
pub use materials::{
    materials_from_ter_mpk, parse_textures_csv, TerrainMaterialLayer, ZoneMaterials,
};
pub use ocean::{append_ocean_plane, ocean_height_from_water_mask, OceanPlane};
pub use shademap::{load_shademap, shademap_from_dat_mpk, water_mask_from_dat_mpk, ShadeMap};
pub use sky_light::{parse_sky_lighting, SkyLighting};

use std::path::Path;
use std::sync::{Mutex, OnceLock};

fn published_lock() -> &'static Mutex<Atmosphere> {
    static LOCK: OnceLock<Mutex<Atmosphere>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(Atmosphere::default()))
}

/// Publish the atmosphere from a region load for the GPU uniform path.
pub fn publish(atm: Atmosphere) {
    if let Ok(mut g) = published_lock().lock() {
        *g = atm;
    }
}

/// Snapshot of the last published atmosphere (empty if never loaded / tables disabled).
#[must_use]
pub fn published() -> Atmosphere {
    published_lock()
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

/// When false, atmosphere code must not open client tables and must not invent the old
/// hardcoded light vector / ambient terms. Empty lighting + no ocean is the correct fallback.
#[must_use]
pub fn tables_enabled() -> bool {
    match std::env::var("CAER_ATMOSPHERE_TABLES") {
        Ok(v) => {
            let v = v.trim();
            !(v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off"))
        }
        Err(_) => true,
    }
}

/// Bundle consumed by the GPU + terrain mesher for one region load.
#[derive(Debug, Clone, Default)]
pub struct Atmosphere {
    pub sky_light: SkyLighting,
    pub lights: Vec<ZoneLight>,
    pub materials: ZoneMaterials,
    /// True when an ocean plane was (or should be) meshed from client water masks.
    pub ocean: Option<OceanPlane>,
}

impl Atmosphere {
    /// Load region-scoped atmosphere from the client tree. Returns a zeroed bundle when
    /// [`tables_enabled`] is false — discriminating against the old hardcoded path.
    pub fn load_for_region(client_root: &Path, region_sky: &str) -> Self {
        if !tables_enabled() {
            return Self::default();
        }
        let sky_mpk = client_root.join("zones/sky/sky.mpk");
        let sky_light = sky_light::load(&sky_mpk, region_sky).unwrap_or_default();
        Self {
            sky_light,
            lights: Vec::new(),
            materials: ZoneMaterials::default(),
            ocean: None,
        }
    }

    /// Merge per-zone lights.csv rows (zone-local XYZ already converted to world by caller).
    pub fn extend_lights(&mut self, zone_lights: &[ZoneLight]) {
        if !tables_enabled() {
            return;
        }
        self.lights.extend_from_slice(zone_lights);
    }

    /// Directional light for shaders: derived from authored point lights (centroid → mean),
    /// with a Z bias so slopes still read. **No hardcoded (0.35,0.25,1) fallback** — empty
    /// lights yield a zero vector and the scene goes flat-dark on the dynamic term.
    #[must_use]
    pub fn light_dir(&self) -> [f32; 3] {
        if self.lights.is_empty() {
            return [0.0, 0.0, 0.0];
        }
        let n = self.lights.len() as f32;
        let cx = self.lights.iter().map(|l| l.x).sum::<f32>() / n;
        let cy = self.lights.iter().map(|l| l.y).sum::<f32>() / n;
        let cz = self.lights.iter().map(|l| l.z).sum::<f32>() / n;
        // Aim from below the light cluster toward it (sun-like), then normalize.
        let mut dx = cx * 0.00001;
        let mut dy = cy * 0.00001;
        let mut dz = (cz * 0.00001).abs() + 1.0;
        // Prefer the mean offset of lights from their centroid as a soft direction cue.
        let (mut ox, mut oy, mut oz) = (0.0f32, 0.0f32, 0.0f32);
        for l in &self.lights {
            ox += l.x - cx;
            oy += l.y - cy;
            oz += (l.z - cz).abs();
        }
        dx += ox / n;
        dy += oy / n;
        dz += oz / n + 1.0;
        let len = (dx * dx + dy * dy + dz * dz).sqrt();
        if len < 1e-6 {
            [0.0, 0.0, 0.0]
        } else {
            [dx / len, dy / len, dz / len]
        }
    }
}

/// Sky file for a pre-world realm stage.
///
/// The character screen has a realm, not a region, so it cannot use [`sky_name_for_region`] — and
/// it was publishing `sky_default` for all three. That is only harmless for Albion, because
/// `sky_albion.dat` and `sky_default.dat` carry identical `lights_and_fog_clear` values. Midgard's
/// table differs from the default in **256** fields and Hibernia's in **76**, so both realms were
/// being lit by Albion's daylight: Midgard's neutral white sun (255,255,255 at 0.5) and Hibernia's
/// paler ambient (160,207,255 at 0.6) were replaced by Albion's (255,225,178 at 0.6 over
/// 178,208,255 at 0.5).
///
/// Named here rather than in the binary so the realm mapping sits beside the region one and there
/// is a single owner for "which sky lights this".
#[must_use]
pub fn sky_name_for_realm(realm: u8) -> &'static str {
    match realm {
        1 => "sky_albion.dat",
        2 => "sky_midgard.dat",
        3 => "sky_hibernia.dat",
        // Realm 0 / unknown: the neutral daylight the client ships for "no region yet".
        _ => "sky_default.dat",
    }
}

/// Default sky file name per region id (overland realms). Unknown regions use `sky_default`.
#[must_use]
pub fn sky_name_for_region(region: u16) -> &'static str {
    match region {
        1 => "sky_albion.dat",
        100..=103 => "sky_albion_frontier",
        200..=299 => "sky_midgard.dat",
        300..=399 => "sky_hibernia",
        30 | 73 | 130 => "sky_oceanus",
        _ => "sky_default",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each realm's stage is lit by its own sky, not by one shared default.
    ///
    /// Seen red against the shipped behaviour, which published `sky_default` for all three. That
    /// is undetectable on Albion — `sky_albion.dat` and `sky_default.dat` carry identical
    /// `lights_and_fog_clear` values — so a gate that only checked Albion would have passed with
    /// the defect fully present. Midgard and Hibernia are where it shows, which is why they are
    /// the assertions that matter here.
    #[test]
    fn every_realm_stage_is_lit_by_its_own_sky() {
        assert_eq!(sky_name_for_realm(1), "sky_albion.dat");
        assert_eq!(sky_name_for_realm(2), "sky_midgard.dat");
        assert_eq!(sky_name_for_realm(3), "sky_hibernia.dat");
        // Realm 0 / unknown keeps the neutral daylight.
        assert_eq!(sky_name_for_realm(0), "sky_default.dat");
        assert_eq!(sky_name_for_realm(9), "sky_default.dat");

        // The control: the three realms must not collapse onto one table. If a future edit
        // pointed them all at the same sky again this fails, whereas checking Albion alone
        // would not.
        let picks = [
            sky_name_for_realm(1),
            sky_name_for_realm(2),
            sky_name_for_realm(3),
        ];
        assert!(
            picks
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == 3,
            "the three realms must resolve three distinct skies, got {picks:?}"
        );
        // And none of them may silently fall back to the neutral default.
        assert!(
            !picks.contains(&"sky_default.dat"),
            "a realm fell back to the neutral sky: {picks:?}"
        );
    }
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn tables_enabled_defaults_on() {
        let _g = env_lock().lock().unwrap();
        std::env::remove_var("CAER_ATMOSPHERE_TABLES");
        assert!(tables_enabled());
    }

    #[test]
    fn tables_flag_off_is_falsifier() {
        let _g = env_lock().lock().unwrap();
        std::env::set_var("CAER_ATMOSPHERE_TABLES", "0");
        assert!(!tables_enabled());
        let atm = Atmosphere::load_for_region(Path::new("/nonexistent"), "sky_albion");
        assert!(atm.lights.is_empty());
        assert_eq!(atm.light_dir(), [0.0, 0.0, 0.0]);
        assert_eq!(atm.sky_light.ambient_amount, 0.0);
        assert_eq!(atm.sky_light.dynamic_amount, 0.0);
        std::env::remove_var("CAER_ATMOSPHERE_TABLES");
    }

    #[test]
    fn light_dir_requires_lights_csv_rows() {
        let mut atm = Atmosphere::default();
        assert_eq!(atm.light_dir(), [0.0, 0.0, 0.0], "no hardcoded sun vector");
        atm.lights.push(ZoneLight {
            x: 1000.0,
            y: 500.0,
            z: 2000.0,
            kind: 9,
        });
        atm.lights.push(ZoneLight {
            x: 3000.0,
            y: 2500.0,
            z: 2100.0,
            kind: 15,
        });
        let d = atm.light_dir();
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        assert!((len - 1.0).abs() < 1e-4, "dir must be unit, got {d:?}");
        assert!(d[2] > 0.0, "biased upward");
    }
}
