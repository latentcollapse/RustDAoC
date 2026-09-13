//! The client's own `default*.ini` — camera defaults, UI panel layout, quickbar config.
//!
//! Eight of these ship at the client root (`default.ini`, `default1024.ini`, … `default2560.ini`)
//! and we never opened one. They are the original's answer to questions we had been answering with
//! invented constants — most visibly the third-person camera, where our hand-picked `CAM_DIST = 200`
//! sits against the client's `distance=500.00`. A 2.5x error in camera distance is most of why a
//! screenshot does not read as DAoC.
//!
//! CAER is a conversion. Where the client ships a number, that number wins.
//!
//! ## Shape
//!
//! Plain INI: `[Section]` then `key=value`. The sections that matter:
//!
//! ```text
//!   [Camera]              distance, height, tilt, angle
//!   [Panels-<W>x<H>]      one entry per window: Name=<comma-separated layout fields>
//!   [Quickbar] [Quickbar2] [Quickbar3]    GroupSize
//!   [Chat] [NameOptions] [ToolTips] [Macros] [QuickBinds]
//! ```
//!
//! `tilt` and `angle` are in DAoC's 0..4096 turn units, the same as entity headings, NOT degrees.
//!
//! Panel entries are positional and their field meanings differ per window (`Help` is 7 numbers,
//! `ChatWindow0` leads with a name then 10), so this parser deliberately keeps them as raw fields
//! rather than inventing a schema. Decoding a given window's fields is a per-window job, done when
//! that window is actually built.

use std::collections::BTreeMap;

/// A parsed client ini: section -> key -> raw value, preserving order within a section.
pub struct ClientIni {
    pub sections: BTreeMap<String, Vec<(String, String)>>,
}

/// The `[Camera]` block: the client's default third-person camera.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraDefaults {
    /// Distance behind the character, world units.
    pub distance: f32,
    /// Height above the character's origin, world units.
    pub height: f32,
    /// Downward tilt in DAoC turn units (0..4096), not degrees.
    pub tilt: i32,
    /// Orbit angle in DAoC turn units (0..4096); 0 is directly behind.
    pub angle: i32,
}

impl CameraDefaults {
    /// Tilt as radians. 4096 units is a full turn.
    #[must_use]
    pub fn tilt_radians(&self) -> f32 {
        self.tilt as f32 * (std::f32::consts::TAU / 4096.0)
    }
    /// Orbit angle as radians.
    #[must_use]
    pub fn angle_radians(&self) -> f32 {
        self.angle as f32 * (std::f32::consts::TAU / 4096.0)
    }
}

impl ClientIni {
    /// Parse INI text. Later duplicate keys within a section are kept (the client itself writes
    /// duplicates, e.g. `LFGClass2` twice), so lookups take the FIRST.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut sections: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        let mut cur = String::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                cur = name.to_string();
                sections.entry(cur.clone()).or_default();
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                sections
                    .entry(cur.clone())
                    .or_default()
                    .push((k.trim().to_string(), v.trim().to_string()));
            }
        }
        Self { sections }
    }

    /// First value for `key` in `section`.
    #[must_use]
    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.sections
            .get(section)?
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    /// The `[Camera]` defaults, if present and parseable.
    #[must_use]
    pub fn camera(&self) -> Option<CameraDefaults> {
        Some(CameraDefaults {
            distance: self.get("Camera", "distance")?.parse().ok()?,
            height: self.get("Camera", "height")?.parse().ok()?,
            tilt: self.get("Camera", "tilt")?.parse().ok()?,
            angle: self.get("Camera", "angle")?.parse().ok()?,
        })
    }

    /// Raw comma-separated fields of one window in `[Panels-<W>x<H>]`.
    ///
    /// Returned as strings because field meanings are per-window; see the module note.
    #[must_use]
    pub fn panel(&self, width: u32, height: u32, window: &str) -> Option<Vec<String>> {
        let sec = format!("Panels-{width}x{height}");
        Some(
            self.get(&sec, window)?
                .split(',')
                .map(|f| f.trim().to_string())
                .collect(),
        )
    }

    /// Every `Panels-WxH` section present, as `(width, height)`.
    #[must_use]
    pub fn panel_resolutions(&self) -> Vec<(u32, u32)> {
        self.sections
            .keys()
            .filter_map(|s| {
                let r = s.strip_prefix("Panels-")?;
                let (w, h) = r.split_once('x')?;
                Some((w.parse().ok()?, h.parse().ok()?))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
[Panels-1024x768]
Version=1
Alpha=100
Help=1,243,96,545,305,100,100
ChatWindow0=Main,0,537,312,195,58,100,10,1,0,1

[Quickbar]
GroupSize=10

[Camera]
distance=500.00
height=10.00
tilt=174
angle=0
";

    #[test]
    fn reads_the_camera_block() {
        let ini = ClientIni::parse(SAMPLE);
        let c = ini.camera().expect("camera section");
        assert_eq!(
            c,
            CameraDefaults {
                distance: 500.0,
                height: 10.0,
                tilt: 174,
                angle: 0
            }
        );
    }

    /// Tilt is in DAoC turn units, not degrees — reading 174 as degrees would put the camera
    /// underground, which is exactly the kind of unit error this project keeps paying for.
    #[test]
    fn tilt_converts_from_turn_units_not_degrees() {
        let ini = ClientIni::parse(SAMPLE);
        let deg = ini.camera().unwrap().tilt_radians().to_degrees();
        assert!(
            (deg - 15.29).abs() < 0.1,
            "174/4096 of a turn is ~15.3 deg, got {deg}"
        );
    }

    #[test]
    fn reads_panel_fields_and_resolutions() {
        let ini = ClientIni::parse(SAMPLE);
        assert_eq!(ini.panel_resolutions(), vec![(1024, 768)]);
        let help = ini.panel(1024, 768, "Help").expect("Help panel");
        assert_eq!(help, vec!["1", "243", "96", "545", "305", "100", "100"]);
        // A leading name field is preserved rather than assumed away.
        let chat = ini.panel(1024, 768, "ChatWindow0").unwrap();
        assert_eq!(chat[0], "Main");
        assert_eq!(chat.len(), 11);
    }

    #[test]
    fn missing_things_are_none_not_defaults() {
        let ini = ClientIni::parse("[Camera]\ndistance=1\n");
        assert!(
            ini.camera().is_none(),
            "a partial Camera block must not be silently defaulted"
        );
        assert!(ini.panel(800, 600, "Help").is_none());
    }
}
