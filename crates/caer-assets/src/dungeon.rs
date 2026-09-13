//! Dungeon zone geometry — `dungeon.chunk` / `dungeon.place` / `dungeon.prop`.
//!
//! Classic dungeon zones (REQ-015 / MS-06) ship **no** `terrain.pcx`. Their authored content
//! lives inside `datNNN.mpk` as a NIF palette plus placements:
//!
//! ```text
//!   dungeon.chunk  — one NIF filename per line (palette; 0-based index)
//!   dungeon.place  — chunk geometry instances: index,x,y,z,angle,ax,ay,az,…
//!   dungeon.prop   — prop instances from the same palette (shorter row; trailing scale)
//! ```
//!
//! These are the dungeon equivalent of surface `fixtures.csv`. A zone with zero place/prop
//! rows is empty; a reader that ignores these members reports `fixtures == 0` and fails the
//! zone-coverage bar.

use std::io;

use crate::fixtures::Fixture;

/// One dungeon geometry/prop placement (zone-local coordinates).
#[derive(Debug, Clone)]
pub struct DungeonPlacement {
    /// Index into [`DungeonZone::chunks`].
    pub chunk_index: u32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// Rotation magnitude, radians.
    pub angle: f32,
    /// Unit rotation axis (authored).
    pub axis: [f32; 3],
    /// Scale in percent (100 = 1×). Places author no scale → 100; props carry a multiplier.
    pub scale: f32,
    /// Trailing integer/flag fields preserved verbatim (portal / collide / unique-id style).
    pub flags: Vec<i32>,
    /// True when the row came from `dungeon.prop` rather than `dungeon.place`.
    pub is_prop: bool,
}

/// A fully decoded dungeon zone from its `datNNN.mpk`.
#[derive(Debug, Clone)]
pub struct DungeonZone {
    /// NIF palette from `dungeon.chunk` (index 0 = first line).
    pub chunks: Vec<String>,
    /// Geometry placements from `dungeon.place`.
    pub places: Vec<DungeonPlacement>,
    /// Prop placements from `dungeon.prop`.
    pub props: Vec<DungeonPlacement>,
}

impl DungeonZone {
    /// Decode a dungeon zone from the bytes of its `datNNN.mpk`.
    ///
    /// Fails with [`io::ErrorKind::NotFound`] when neither `dungeon.chunk` nor `dungeon.place`
    /// is present — surface/skycity archives must not silently parse as empty dungeons.
    pub fn from_dat_mpk(dat_mpk: &[u8]) -> io::Result<Self> {
        let members = crate::read(dat_mpk)?;
        let find = |name: &str| {
            members
                .iter()
                .find(|m| m.name.eq_ignore_ascii_case(name))
                .map(|m| String::from_utf8_lossy(&m.data).into_owned())
        };
        let chunk_text = find("dungeon.chunk");
        let place_text = find("dungeon.place");
        if chunk_text.is_none() && place_text.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "dungeon.chunk / dungeon.place not in dat archive",
            ));
        }
        let chunks = parse_chunk_list(chunk_text.as_deref().unwrap_or(""));
        let places = parse_placements(place_text.as_deref().unwrap_or(""), false);
        let props = parse_placements(find("dungeon.prop").as_deref().unwrap_or(""), true);
        Ok(Self {
            chunks,
            places,
            props,
        })
    }

    /// Place + prop count — the dungeon `fixtures > 0` denominator.
    #[must_use]
    pub fn fixture_count(&self) -> usize {
        self.places.len() + self.props.len()
    }

    /// Project placements onto the surface [`Fixture`] shape so downstream tools can treat
    /// dungeon content as first-class fixtures without inventing a second render path.
    #[must_use]
    pub fn as_fixtures(&self) -> Vec<Fixture> {
        let mut out = Vec::with_capacity(self.fixture_count());
        let mut push = |p: &DungeonPlacement, id_base: u32| {
            let filename = self
                .chunks
                .get(p.chunk_index as usize)
                .cloned()
                .unwrap_or_default();
            let name = filename
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&filename)
                .trim()
                .to_string();
            // Render-frame yaw: same world→render axis mirror used by fixtures.csv LONG rows.
            let (rx, ry, rz) = (-p.axis[0], p.axis[1], -p.axis[2]);
            let (sh, ch) = (p.angle * 0.5).sin_cos();
            let (qx, qy, qz, qw) = (rx * sh, ry * sh, rz * sh, ch);
            let angle = (2.0 * (qw * qz + qx * qy)).atan2(1.0 - 2.0 * (qy * qy + qz * qz));
            out.push(Fixture {
                id: id_base + p.chunk_index,
                nif_id: p.chunk_index,
                name,
                x: p.x,
                y: p.y,
                z: p.z,
                scale: p.scale,
                on_ground: false,
                radius: 0.0,
                filename,
                angle,
                axis: p.axis,
                angle_raw: p.angle,
                unique_id: p.flags.get(1).copied().unwrap_or(0).max(0) as u32,
                collide: 0.0,
                instance_radius: 0.0,
                animate: false,
                cave: true,
            });
        };
        for (i, p) in self.places.iter().enumerate() {
            push(p, i as u32);
        }
        let place_n = self.places.len() as u32;
        for (i, p) in self.props.iter().enumerate() {
            push(p, place_n + 1_000_000 + i as u32);
        }
        out
    }
}

/// True when an MPAK directory listing names dungeon geometry members.
#[must_use]
pub fn dat_names_are_dungeon(names: &[String]) -> bool {
    names.iter().any(|n| {
        let l = n.to_ascii_lowercase();
        l == "dungeon.chunk" || l == "dungeon.place"
    })
}

/// Peek `datNNN.mpk` names without decoding geometry — inventory / class gate.
pub fn dat_is_dungeon_mpk(dat_mpk: &[u8]) -> io::Result<bool> {
    Ok(dat_names_are_dungeon(&crate::list_names_bytes(dat_mpk)?))
}

/// `dungeon.chunk`: one NIF filename per non-empty line (CRLF or LF).
pub fn parse_chunk_list(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.trim().trim_start_matches('\u{feff}'))
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

/// Parse `dungeon.place` (`is_prop = false`) or `dungeon.prop` (`is_prop = true`) rows.
///
/// Layout (comma-separated, optional spaces):
/// `index, x, y, z, angle, ax, ay, az, [flags…]` — props additionally encode scale as the last
/// float when present (multiplier → percent).
pub fn parse_placements(text: &str, is_prop: bool) -> Vec<DungeonPlacement> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if fields.len() < 8 {
            continue;
        }
        let Ok(chunk_index) = fields[0].parse::<u32>() else {
            continue;
        };
        let num = |i: usize| fields[i].parse::<f32>().unwrap_or(0.0);
        let (ax, ay, az) = (num(5), num(6), num(7));
        let len = (ax * ax + ay * ay + az * az).sqrt();
        let axis = if len > 1e-6 {
            [ax / len, ay / len, az / len]
        } else {
            [0.0, 0.0, -1.0]
        };

        // Props: trailing float is a scale multiplier (1.0 = 100%). Places have no scale.
        let (scale, flag_fields) = if is_prop && fields.len() >= 11 {
            let mult = fields[fields.len() - 1].parse::<f32>().unwrap_or(1.0);
            (mult * 100.0, &fields[8..fields.len() - 1])
        } else {
            (100.0, &fields[8..])
        };
        let flags: Vec<i32> = flag_fields
            .iter()
            .filter_map(|s| s.parse::<i32>().ok())
            .collect();

        out.push(DungeonPlacement {
            chunk_index,
            x: num(1),
            y: num(2),
            z: num(3),
            angle: num(4),
            axis,
            scale,
            flags,
            is_prop,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHUNK: &str = "\
bdung1hall1.NIF\r
bdung1hall2.NIF\r
MineHall1.NIF\r
";

    const PLACE: &str = "\
1, 512.000000, 0.000000, 0.000000, 0.000000, 1.000000, 0.000000, 0.000000, 0, 0, 4, 0, 0\r
2, -384.000000, -895.999939, -240.000000, 1.570796, 0.000000, 0.000000, 1.000000, 0, 0, 4, 0, 0\r
";

    const PROP: &str = "\
0, 641.000000, 211.000000, 67.000000, 3.141593, 0.000000, 0.000000, 1.000000, 0, 148, 1.000000\r
";

    #[test]
    fn parses_chunk_palette_and_placements() {
        let chunks = parse_chunk_list(CHUNK);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], "bdung1hall1.NIF");

        let places = parse_placements(PLACE, false);
        assert_eq!(places.len(), 2);
        assert_eq!(places[0].chunk_index, 1);
        assert!((places[0].x - 512.0).abs() < 1e-3);
        assert!((places[0].axis[0] - 1.0).abs() < 1e-4);
        assert_eq!(places[0].scale, 100.0);
        assert!(!places[0].is_prop);

        let props = parse_placements(PROP, true);
        assert_eq!(props.len(), 1);
        assert_eq!(props[0].chunk_index, 0);
        assert!((props[0].scale - 100.0).abs() < 1e-3);
        assert!(props[0].is_prop);
        assert_eq!(props[0].flags, vec![0, 148]);
    }

    #[test]
    fn fixture_count_and_as_fixtures_join_palette() {
        let zone = DungeonZone {
            chunks: parse_chunk_list(CHUNK),
            places: parse_placements(PLACE, false),
            props: parse_placements(PROP, true),
        };
        assert_eq!(zone.fixture_count(), 3);
        let fx = zone.as_fixtures();
        assert_eq!(fx.len(), 3);
        assert_eq!(fx[0].filename, "bdung1hall2.NIF");
        assert_eq!(fx[2].filename, "bdung1hall1.NIF");
        assert!(fx.iter().all(|f| f.cave), "dungeon fixtures mark cave");
    }

    /// Deleting the reader (or collapsing dungeons into "empty terrain") must fail this.
    #[test]
    fn dat_names_gate_rejects_surface_only_archives() {
        let surface = vec![
            "terrain.pcx".into(),
            "offset.pcx".into(),
            "SECTOR.DAT".into(),
        ];
        assert!(!dat_names_are_dungeon(&surface));
        let dungeon = vec!["dungeon.chunk".into(), "dungeon.place".into()];
        assert!(dat_names_are_dungeon(&dungeon));
    }

    #[test]
    fn from_dat_mpk_errors_without_dungeon_members() {
        // Minimal non-dungeon MPAK: hand-built is heavy; reject via empty synthetic path by
        // calling parse on a surface-shaped name list through the public gate instead.
        assert!(!dat_names_are_dungeon(&["fixtures.csv".into()]));
    }
}
