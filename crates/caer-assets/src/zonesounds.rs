//! `sounds.dat` — a zone's ambient sound regions.
//!
//! 447 of these ship (one per zone) and we never opened one; sound is the largest subsystem still
//! entirely unstarted. This decodes the placement/scheduling data so the eventual audio backend has
//! the client's own definitions to work from rather than an invented ambience scheme.
//!
//! ## Format
//!
//! INI-like, CRLF, with a trailing comment on the section header carrying the region's tags:
//!
//! ```text
//!   ; ZONE 002
//!   [SoundRegion00];---[terrain:prairie:weather]
//!   sound_type=1
//!   fade_time=10
//!   zone_wide_sounds=0
//!
//!   ; SOUNDS
//!   Sound00=2, s_Bed_Prairie, 0, 100, 24, 0, 0, 0
//!   Sound01=1, g_Wind_Prairie, 2-6, 100, 64, 0431, 0530, 1
//!   ; SHAPES
//!   Shape00=0, 0, 0, 65535, 65535
//! ```
//!
//! A `SoundNN` row is: kind, logical name, spacing (a literal or an `a-b` range, in seconds between
//! plays), volume, radius, start time, end time (`HHMM`, `0`/`0` meaning always), and a weather id.
//! `ShapeNN` bounds the region; `0,0,0,65535,65535` covers the whole zone.
//!
//! ## What is NOT decoded, and is not guessed
//!
//! The logical names (`s_Bed_Prairie`, `g_Wind_Prairie`) do not resolve to shipped files by any
//! rule verified here. `g_Thunder` plainly corresponds to `sounds/thunder1..6.wav` — prefix stripped,
//! numbered variants — but no `*Prairie*` file exists in any sound directory, the name appears in no
//! table or archive, and it is absent from `game1127.dll`'s strings. So the prefix meaning
//! (`s_` vs `g_`) and the name-to-file rule are recorded as open rather than assumed. Decoding the
//! schedule is useful on its own; inventing a filename rule would be the exact failure this project
//! is trying to stop repeating.

use std::collections::BTreeMap;

/// How often a sound plays, in seconds between plays.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Spacing {
    /// A single value; `0` means continuous (the ambient "bed").
    Fixed(u32),
    /// A random interval in `min..=max`.
    Range(u32, u32),
}

/// One scheduled sound within a region.
#[derive(Debug, Clone, PartialEq)]
pub struct ZoneSound {
    /// Leading field. 2 is used for continuous beds, 1 for intermittent one-shots; the full
    /// enumeration is not established, so it is kept as the raw number.
    pub kind: u32,
    /// Logical name as written (`"s_Bed_Prairie"`). See the module note: this does NOT resolve to a
    /// file by any rule verified here.
    pub name: String,
    pub spacing: Spacing,
    pub volume: u32,
    pub radius: u32,
    /// Start/end of day as `HHMM`; `0`/`0` means always.
    pub start_time: u32,
    pub end_time: u32,
    /// Weather id this sound belongs to (0 = clear).
    pub weather: u32,
}

/// A rectangular area a region covers. `0,0,0,65535,65535` is the whole zone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SoundShape {
    pub kind: u32,
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

/// One `[SoundRegionNN]`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SoundRegion {
    /// Tags from the header's trailing comment, e.g. `["terrain", "prairie", "weather"]`.
    pub tags: Vec<String>,
    /// Scalar settings (`fade_time`, `zone_wide_sounds`, …), kept raw: their meanings are not all
    /// established and a partial enum would lose the rest.
    pub settings: BTreeMap<String, String>,
    pub sounds: Vec<ZoneSound>,
    pub shapes: Vec<SoundShape>,
}

/// A parsed `sounds.dat`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ZoneSounds {
    pub regions: Vec<SoundRegion>,
}

impl ZoneSounds {
    /// Total scheduled sounds across every region.
    #[must_use]
    pub fn sound_count(&self) -> usize {
        self.regions.iter().map(|r| r.sounds.len()).sum()
    }

    /// Every distinct logical sound name referenced, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .regions
            .iter()
            .flat_map(|r| r.sounds.iter().map(|s| s.name.as_str()))
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

fn parse_spacing(s: &str) -> Option<Spacing> {
    let s = s.trim();
    if let Some((a, b)) = s.split_once('-') {
        return Some(Spacing::Range(
            a.trim().parse().ok()?,
            b.trim().parse().ok()?,
        ));
    }
    Some(Spacing::Fixed(s.parse().ok()?))
}

/// Parse a `sounds.dat`. Unparseable rows are skipped rather than failing the file: these are
/// hand-authored across 447 zones and one malformed line should not cost a zone its ambience.
#[must_use]
pub fn parse(text: &str) -> ZoneSounds {
    let mut out = ZoneSounds::default();
    let mut cur: Option<SoundRegion> = None;

    for raw in text.lines() {
        let line = raw.trim_end_matches(['\r', '\n']).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("[SoundRegion") {
            if let Some(r) = cur.take() {
                out.regions.push(r);
            }
            // Tags live in the trailing comment as [a:b:c].
            let tags = line
                .rsplit_once('[')
                .and_then(|(_, t)| t.strip_suffix(']'))
                .map(|t| {
                    t.split(':')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            cur = Some(SoundRegion {
                tags,
                ..SoundRegion::default()
            });
            continue;
        }
        // A standalone comment line; section headers are handled above and keep their comment.
        if line.starts_with(';') {
            continue;
        }
        let Some(region) = cur.as_mut() else { continue };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.split(';').next().unwrap_or("").trim());

        if let Some(rest) = key.strip_prefix("Sound") {
            if rest.chars().all(|c| c.is_ascii_digit()) {
                let f: Vec<&str> = value.split(',').map(str::trim).collect();
                if f.len() >= 8 {
                    if let (Ok(kind), Some(spacing)) = (f[0].parse::<u32>(), parse_spacing(f[2])) {
                        region.sounds.push(ZoneSound {
                            kind,
                            name: f[1].to_string(),
                            spacing,
                            volume: f[3].parse().unwrap_or(0),
                            radius: f[4].parse().unwrap_or(0),
                            start_time: f[5].parse().unwrap_or(0),
                            end_time: f[6].parse().unwrap_or(0),
                            weather: f[7].parse().unwrap_or(0),
                        });
                    }
                }
                continue;
            }
        }
        if let Some(rest) = key.strip_prefix("Shape") {
            if rest.chars().all(|c| c.is_ascii_digit()) {
                let f: Vec<u32> = value
                    .split(',')
                    .filter_map(|v| v.trim().parse().ok())
                    .collect();
                if f.len() >= 5 {
                    region.shapes.push(SoundShape {
                        kind: f[0],
                        x0: f[1],
                        y0: f[2],
                        x1: f[3],
                        y1: f[4],
                    });
                }
                continue;
            }
        }
        region.settings.insert(key.to_string(), value.to_string());
    }
    if let Some(r) = cur.take() {
        out.regions.push(r);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "; ZONE 002\r
[SoundRegion00];-----[terrain:prairie:weather]\r
sound_type=1\r
fade_time=10\r
zone_wide_sounds=0\r
\r
; SOUNDS \r
Sound00=2, s_Bed_Prairie, 0, 100, 24, 0, 0, 0 \r
Sound01=1, g_Wind_Prairie, 2-6, 100, 64, 0431, 0530, 1 \r
; SHAPES \r
Shape00=0, 0, 0, 65535, 65535\r
\r
[SoundRegion01];-----[terrain:prairie:animals]\r
sound_type=1\r
Sound00=1, g_Bird, 10-30, 80, 96, 0600, 1900, 0 \r
";

    #[test]
    fn splits_regions_and_reads_their_tags() {
        let z = parse(SAMPLE);
        assert_eq!(z.regions.len(), 2);
        assert_eq!(z.regions[0].tags, vec!["terrain", "prairie", "weather"]);
        assert_eq!(z.regions[1].tags, vec!["terrain", "prairie", "animals"]);
    }

    /// Spacing is either a fixed value or a range; conflating them would turn an intermittent
    /// bird call into a continuous drone.
    #[test]
    fn reads_sound_rows_including_ranged_spacing() {
        let z = parse(SAMPLE);
        let s = &z.regions[0].sounds;
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, "s_Bed_Prairie");
        assert_eq!(s[0].spacing, Spacing::Fixed(0));
        assert_eq!(s[0].radius, 24);
        assert_eq!(s[1].spacing, Spacing::Range(2, 6));
        assert_eq!((s[1].start_time, s[1].end_time), (431, 530));
        assert_eq!(s[1].weather, 1);
    }

    #[test]
    fn reads_shapes_and_keeps_settings_raw() {
        let z = parse(SAMPLE);
        assert_eq!(
            z.regions[0].shapes,
            vec![SoundShape {
                kind: 0,
                x0: 0,
                y0: 0,
                x1: 65535,
                y1: 65535
            }]
        );
        assert_eq!(
            z.regions[0].settings.get("fade_time").map(String::as_str),
            Some("10")
        );
        // Sound/Shape rows must NOT leak into settings.
        assert!(!z.regions[0].settings.contains_key("Sound00"));
    }

    #[test]
    fn aggregates_across_regions() {
        let z = parse(SAMPLE);
        assert_eq!(z.sound_count(), 3);
        assert_eq!(z.names(), vec!["g_Bird", "g_Wind_Prairie", "s_Bed_Prairie"]);
    }

    /// A malformed row must not cost the file its other sounds.
    #[test]
    fn a_bad_row_is_skipped_not_fatal() {
        let z = parse("[SoundRegion00];[x]\nSound00=oops\nSound01=1, g_Bird, 5, 80, 96, 0, 0, 0\n");
        assert_eq!(z.regions[0].sounds.len(), 1);
        assert_eq!(z.regions[0].sounds[0].name, "g_Bird");
    }
}
