//! `lights.csv` — per-zone point lights inside `datNNN.mpk` / `csvNNN.mpk`.
//!
//! Rows are `x,y,z,type` in zone-local world units (same space as fixtures). There is no header.

use std::io;

/// One authored point light.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZoneLight {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// Client light-type id (torch, lamp, …). Preserved for later typed radii; direction
    /// aggregation treats all kinds equally for now.
    pub kind: u16,
}

/// Lights for one zone, still in zone-local coordinates.
#[derive(Debug, Clone, Default)]
pub struct ZoneLights {
    pub lights: Vec<ZoneLight>,
}

/// Parse a `lights.csv` member (raw CSV bytes).
pub fn parse_lights_csv(bytes: &[u8]) -> io::Result<ZoneLights> {
    let text = String::from_utf8_lossy(bytes);
    let mut lights = Vec::new();
    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split(',');
        let Some(x) = parts.next().and_then(|s| s.trim().parse().ok()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("lights.csv:{lineno}: bad x"),
            ));
        };
        let Some(y) = parts.next().and_then(|s| s.trim().parse().ok()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("lights.csv:{lineno}: bad y"),
            ));
        };
        let Some(z) = parts.next().and_then(|s| s.trim().parse().ok()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("lights.csv:{lineno}: bad z"),
            ));
        };
        let kind = parts
            .next()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        lights.push(ZoneLight { x, y, z, kind });
    }
    Ok(ZoneLights { lights })
}

/// Pull `lights.csv` from a zone `dat`/`csv` MPAK.
pub fn lights_from_mpk(mpk: &[u8]) -> io::Result<ZoneLights> {
    let members = caer_assets::read(mpk)?;
    let Some(entry) = members
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case("lights.csv"))
    else {
        return Ok(ZoneLights::default());
    };
    parse_lights_csv(&entry.data)
}

/// Translate zone-local lights into world XYZ using the zone's grid origin.
#[must_use]
pub fn to_world(lights: &ZoneLights, zone_origin_xy: [f32; 2]) -> Vec<ZoneLight> {
    lights
        .lights
        .iter()
        .map(|l| ZoneLight {
            x: l.x + zone_origin_xy[0],
            y: l.y + zone_origin_xy[1],
            z: l.z,
            kind: l.kind,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_zone001_shape() {
        let csv = b"36462,4578,3044,9\n9266,52384,2393,3\n";
        let z = parse_lights_csv(csv).unwrap();
        assert_eq!(z.lights.len(), 2);
        assert_eq!(z.lights[0].x, 36462.0);
        assert_eq!(z.lights[0].kind, 9);
        assert_eq!(z.lights[1].kind, 3);
    }

    #[test]
    fn empty_csv_is_ok() {
        assert!(parse_lights_csv(b"").unwrap().lights.is_empty());
    }
}
