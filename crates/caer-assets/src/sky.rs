//! Sky definitions — the client's own per-region sky colours (`zones/sky/sky.mpk`).
//!
//! Every region names a sky file (`sky_albion.dat`, `sky_midgard.dat`, …) and they are **plain
//! text INI**, not a binary format — 100% printable bytes. So the sky does not need reverse
//! engineering at all; it needs reading. That is the difference between guessing a gradient and
//! rendering the one the game shipped.
//!
//! What matters for a sky dome is two colours per time of day:
//!
//! ```text
//! [canopy_color_clear]    day_zenith = 82,134,217,255      ← straight overhead
//! [lights_and_fog_clear]  day_distance_fog = 141,170,217   ← at the horizon
//! ```
//!
//! The fog colour doubles as the horizon colour, which is exactly how the original looks: the sky
//! meets the ground in the same haze the distance fades into. The files carry a great deal more
//! (sun disk/flare/glow textures and scales, moon phases, cloud layers, storm variants); this
//! reads the subset a gradient dome needs and leaves the rest for when those land.

use std::collections::HashMap;
use std::io;

/// An RGB colour, 0–255.
pub type Rgb = [u8; 3];

/// Zenith + horizon for one time of day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkyBand {
    /// Colour straight up.
    pub zenith: Rgb,
    /// Colour at the horizon — the client's distance-fog colour, which is what the sky fades to.
    pub horizon: Rgb,
}

/// One region's sky, at the four times of day the client defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sky {
    pub dawn: SkyBand,
    pub day: SkyBand,
    pub dusk: SkyBand,
    pub night: SkyBand,
}

impl Default for Sky {
    /// Albion's clear-weather colours, as shipped. Used when a region's sky file is missing so the
    /// sky is never the flat void it was before.
    fn default() -> Self {
        Self {
            dawn: SkyBand {
                zenith: [28, 55, 99],
                horizon: [131, 100, 104],
            },
            day: SkyBand {
                zenith: [82, 134, 217],
                horizon: [141, 170, 217],
            },
            dusk: SkyBand {
                zenith: [82, 134, 217],
                horizon: [115, 113, 127],
            },
            night: SkyBand {
                zenith: [5, 18, 33],
                horizon: [6, 20, 38],
            },
        }
    }
}

impl Sky {
    /// Interpolate the sky for a normalised time of day (`0.0`–`1.0`, midnight to midnight).
    ///
    /// Anchors at night 0.0, dawn 0.25, day 0.5, dusk 0.75, back to night at 1.0, blending
    /// linearly between them so the sky slides rather than snapping between four presets.
    #[must_use]
    pub fn at(&self, t: f32) -> SkyBand {
        let t = t.rem_euclid(1.0);
        let stops = [
            (0.0, self.night),
            (0.25, self.dawn),
            (0.5, self.day),
            (0.75, self.dusk),
            (1.0, self.night),
        ];
        for w in stops.windows(2) {
            let (t0, a) = w[0];
            let (t1, b) = w[1];
            if t >= t0 && t <= t1 {
                let k = if (t1 - t0).abs() < f32::EPSILON {
                    0.0
                } else {
                    (t - t0) / (t1 - t0)
                };
                return SkyBand {
                    zenith: mix(a.zenith, b.zenith, k),
                    horizon: mix(a.horizon, b.horizon, k),
                };
            }
        }
        self.day
    }
}

fn mix(a: Rgb, b: Rgb, k: f32) -> Rgb {
    let k = k.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| (f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * k).round() as u8)
}

/// Parse `r,g,b[,a]`; the alpha the files carry on canopy colours is ignored (the dome is opaque).
fn parse_rgb(v: &str) -> Option<Rgb> {
    let mut it = v.split(',').map(|p| p.trim().parse::<i32>().ok());
    let r = it.next()??;
    let g = it.next()??;
    let b = it.next()??;
    Some([
        r.clamp(0, 255) as u8,
        g.clamp(0, 255) as u8,
        b.clamp(0, 255) as u8,
    ])
}

/// Parse a sky `.dat` (INI) into its colour bands.
///
/// Only the CLEAR-weather sections are read — storm variants are a weather feature, and picking
/// them by default would make every zone permanently overcast.
pub fn parse(text: &str) -> Sky {
    let mut section = String::new();
    let mut keys: HashMap<(String, String), String> = HashMap::new();
    for line in text.lines() {
        let line = line.split(';').next().unwrap_or("").trim(); // ';' starts a comment
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

    let get = |sec: &str, key: &str| {
        keys.get(&(sec.to_string(), key.to_string()))
            .and_then(|v| parse_rgb(v))
    };
    let defaults = Sky::default();
    // One band per time of day, each falling back to the shipped Albion colour for anything the
    // file omits.
    let band = |name: &str, fallback: SkyBand| SkyBand {
        zenith: get("canopy_color_clear", &format!("{name}_zenith")).unwrap_or(fallback.zenith),
        horizon: get("lights_and_fog_clear", &format!("{name}_distance_fog"))
            .unwrap_or(fallback.horizon),
    };
    Sky {
        dawn: band("dawn", defaults.dawn),
        day: band("day", defaults.day),
        dusk: band("dusk", defaults.dusk),
        night: band("night", defaults.night),
    }
}

/// Load a region's sky from the client's `zones/sky/sky.mpk`.
///
/// `region_file` is the sky name a region asks for (e.g. `"sky_albion"`); the `.dat` suffix is
/// added if absent. Falls back to `sky_default`, then to [`Sky::default`], so a missing or unknown
/// region still gets a real sky rather than a void.
pub fn load(sky_mpk: impl AsRef<std::path::Path>, region_file: &str) -> io::Result<Sky> {
    let entries = crate::open(sky_mpk)?;
    let want = if region_file.to_ascii_lowercase().ends_with(".dat") {
        region_file.to_ascii_lowercase()
    } else {
        format!("{}.dat", region_file.to_ascii_lowercase())
    };
    let member = entries
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(&want))
        .or_else(|| {
            entries
                .iter()
                .find(|e| e.name.eq_ignore_ascii_case("sky_default.dat"))
        });
    Ok(member.map_or_else(Sky::default, |e| parse(&String::from_utf8_lossy(&e.data))))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim shape of `sky_albion.dat`, including the comment lines and the 4-component canopy
    /// colours the real file uses.
    const ALBION: &str = "\
;------------------------------------------------------------------------------
;   ALBION
[main]
data_path           =zones/sky
[lights_and_fog_clear]
dawn_distance_fog  =131,100,104
day_distance_fog   =141,170,217
dusk_distance_fog  =115,113,127
night_distance_fog =6,20,38
[lights_and_fog_stormy]
day_distance_fog   =128,128,128
[canopy_color_clear]
dawn_zenith=28,55,99,128
day_zenith=82,134,217,255
dusk_zenith=82,134,217,255
night_zenith=5,18,33,255
[canopy_color_stormy]
day_zenith=160,160,160,255
";

    #[test]
    fn parses_the_shipped_albion_sky() {
        let s = parse(ALBION);
        assert_eq!(s.day.zenith, [82, 134, 217], "daytime dome colour");
        assert_eq!(
            s.day.horizon,
            [141, 170, 217],
            "horizon comes from the distance fog"
        );
        assert_eq!(s.night.zenith, [5, 18, 33]);
        assert_eq!(s.night.horizon, [6, 20, 38]);
        assert_eq!(
            s.dawn.zenith,
            [28, 55, 99],
            "4-component canopy colour drops its alpha"
        );
    }

    /// Storm sections must NOT win — reading them by default would leave every zone overcast.
    #[test]
    fn storm_variants_are_ignored() {
        let s = parse(ALBION);
        assert_ne!(
            s.day.zenith,
            [160, 160, 160],
            "picked up the stormy canopy colour"
        );
        assert_ne!(
            s.day.horizon,
            [128, 128, 128],
            "picked up the stormy fog colour"
        );
    }

    /// Comments and blank lines must not be mistaken for keys.
    #[test]
    fn comments_are_stripped() {
        let s = parse("[canopy_color_clear]\nday_zenith=1,2,3 ; trailing comment\n;whole line\n");
        assert_eq!(s.day.zenith, [1, 2, 3]);
    }

    /// Time of day blends between the four anchors rather than snapping.
    #[test]
    fn time_of_day_interpolates() {
        let s = parse(ALBION);
        assert_eq!(s.at(0.5).zenith, s.day.zenith, "noon is the day colour");
        assert_eq!(
            s.at(0.0).zenith,
            s.night.zenith,
            "midnight is the night colour"
        );

        // Halfway from dawn to day is between the two, not equal to either.
        let mid = s.at(0.375).zenith;
        assert!(
            mid != s.dawn.zenith && mid != s.day.zenith,
            "should blend, got {mid:?}"
        );
        for (i, &ch) in mid.iter().enumerate() {
            let (lo, hi) = (
                s.dawn.zenith[i].min(s.day.zenith[i]),
                s.dawn.zenith[i].max(s.day.zenith[i]),
            );
            assert!(
                (lo..=hi).contains(&ch),
                "channel {i} outside the blend range"
            );
        }
        // Wrapping past midnight must not panic or produce a discontinuity.
        assert_eq!(s.at(1.0).zenith, s.at(0.0).zenith);
    }

    /// A region with no sky file still gets real colours, never a void.
    #[test]
    fn default_is_a_real_sky() {
        let d = Sky::default();
        assert_eq!(d.day.zenith, [82, 134, 217]);
        assert_ne!(d.day.zenith, [0, 0, 0]);
    }
}
