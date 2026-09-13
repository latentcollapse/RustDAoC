//! Sky `lights_and_fog_*` lighting terms from `zones/sky/sky.mpk` (INI `.dat` members).
//!
//! `caer_assets::sky` already reads zenith/horizon colours; this module additionally reads the
//! ambient/dynamic light colours and amounts that replace the old shader hardcodes. Kept inside
//! `caer-render` (Stream C ownership) rather than extending `caer-assets`.

use std::collections::HashMap;
use std::io;
use std::path::Path;

/// One time-of-day's lighting from `lights_and_fog_clear`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkyLighting {
    pub ambient: [f32; 3],
    pub ambient_amount: f32,
    pub dynamic: [f32; 3],
    pub dynamic_amount: f32,
}

impl Default for SkyLighting {
    /// Empty — **not** the old hardcoded sun. Used when tables are disabled or missing.
    fn default() -> Self {
        Self {
            ambient: [0.0, 0.0, 0.0],
            ambient_amount: 0.0,
            dynamic: [0.0, 0.0, 0.0],
            dynamic_amount: 0.0,
        }
    }
}

impl SkyLighting {
    /// Daytime band used for the static viewer until time-of-day wiring lands.
    #[must_use]
    pub fn for_upload(&self) -> Self {
        *self
    }
}

/// Parse clear-weather day lighting out of a sky `.dat` body.
pub fn parse_sky_lighting(text: &str) -> SkyLighting {
    let mut section = String::new();
    let mut keys: HashMap<(String, String), String> = HashMap::new();
    for line in text.lines() {
        let line = line.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_ascii_lowercase();
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            keys.insert(
                (section.clone(), k.trim().to_ascii_lowercase()),
                v.trim().to_string(),
            );
        }
    }
    let get_rgb = |sec: &str, key: &str| -> Option<[f32; 3]> {
        let v = keys.get(&(sec.to_string(), key.to_string()))?;
        let mut it = v.split(',').map(|p| p.trim().parse::<f32>().ok());
        let r = it.next()??;
        let g = it.next()??;
        let b = it.next()??;
        Some([r / 255.0, g / 255.0, b / 255.0])
    };
    let get_f = |sec: &str, key: &str| -> Option<f32> {
        keys.get(&(sec.to_string(), key.to_string()))?.parse().ok()
    };
    const SEC: &str = "lights_and_fog_clear";
    SkyLighting {
        ambient: get_rgb(SEC, "day_ambient_light").unwrap_or([0.0, 0.0, 0.0]),
        ambient_amount: get_f(SEC, "day_ambient_light_amount").unwrap_or(0.0),
        dynamic: get_rgb(SEC, "day_dynamic_light").unwrap_or([0.0, 0.0, 0.0]),
        dynamic_amount: get_f(SEC, "day_dynamic_light_amount").unwrap_or(0.0),
    }
}

/// Load lighting for `region_file` from `sky.mpk`.
pub fn load(sky_mpk: &Path, region_file: &str) -> io::Result<SkyLighting> {
    let want = if region_file.to_ascii_lowercase().ends_with(".dat") {
        region_file.to_ascii_lowercase()
    } else {
        format!("{}.dat", region_file.to_ascii_lowercase())
    };
    let member = caer_assets::open_named(sky_mpk, &[want.as_str(), "sky_default.dat"])?
        .into_iter()
        .next();
    Ok(member.map_or_else(SkyLighting::default, |e| {
        parse_sky_lighting(&String::from_utf8_lossy(&e.data))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALBION_LIGHT: &str = "\
[lights_and_fog_clear]
day_ambient_light =178,208,255
day_ambient_light_amount =0.5
day_dynamic_light =255,225,178
day_dynamic_light_amount =0.6
day_distance_fog   =141,170,217
[lights_and_fog_stormy]
day_ambient_light =255,255,255
day_ambient_light_amount =0.6
day_dynamic_light_amount =0.0
";

    #[test]
    fn parses_albion_day_lighting() {
        let l = parse_sky_lighting(ALBION_LIGHT);
        assert!((l.ambient_amount - 0.5).abs() < 1e-5);
        assert!((l.dynamic_amount - 0.6).abs() < 1e-5);
        assert!((l.ambient[0] - 178.0 / 255.0).abs() < 1e-4);
        assert!((l.dynamic[0] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn storm_section_ignored() {
        let l = parse_sky_lighting(ALBION_LIGHT);
        assert!((l.dynamic_amount - 0.6).abs() < 1e-5);
        assert_ne!(l.ambient_amount, 0.6);
    }

    #[test]
    fn missing_keys_are_zero_not_hardcoded() {
        let l = parse_sky_lighting("[lights_and_fog_clear]\n");
        assert_eq!(l, SkyLighting::default());
    }
}
