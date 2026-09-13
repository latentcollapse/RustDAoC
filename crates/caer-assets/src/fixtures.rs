//! Zone fixture placements — the static world population (trees, houses, keeps, towers…).
//!
//! Each zone's `csvNNN.mpk` carries two tables:
//!
//! * `nifs.csv`   — the zone's model palette: NIF id → filename, collide radius, etc.
//! * `fixtures.csv` — placed instances: NIF id, zone-local X/Y/Z, angle, scale (percent),
//!   and a `Ground` flag (1 = snap the fixture's Z to the terrain at load time).
//!
//! Until the NIF mesh parser exists, the renderer draws these as placeholder boxes sized from
//! the model's collide radius × the instance scale — enough to make villages, keeps and forests
//! read as structure on the terrain.

use std::collections::HashMap;
use std::io;

/// One model palette entry from `nifs.csv`.
pub struct NifDef {
    pub name: String,
    pub filename: String,
    /// Collide radius, world units (pre-scale).
    pub radius: f32,
}

/// One placed fixture from `fixtures.csv`, coordinates zone-local.
#[derive(Clone)]
pub struct Fixture {
    /// The row's own `ID` (col 0) — unique within the zone. Paired with the zone id it forms a
    /// stable key the editor uses to persist per-fixture overrides (rotation today, more later).
    pub id: u32,
    pub nif_id: u32,
    pub name: String,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// Scale in percent (100 = 1×).
    pub scale: f32,
    /// Snap Z to the terrain height at (x, y).
    pub on_ground: bool,
    /// Collide radius from the palette entry, world units (pre-scale). 0 if unknown.
    pub radius: f32,
    /// Model file from the palette (e.g. `"elm2.nif"`). Empty if the palette lacks the id.
    pub filename: String,
    /// Rotation around +Z, radians (from the 3D Angle/Axis columns).
    pub angle: f32,
    /// The FULL authored rotation axis, unit length, as written in cols 16..18. Kept because the
    /// engine's "fixtures only yaw" assumption is false: 2,493 region-1 rows author a nonzero X
    /// component and 254 a nonzero Y, which is a deliberate pitch/roll (leaning stones, wrecked
    /// boats, tents, burnt trees). `angle` collapses that to a yaw; this preserves the original so
    /// the renderer can stop lying about it. `[0,0,±1]` for the ordinary yaw-only case.
    pub axis: [f32; 3],
    /// The raw authored rotation magnitude in radians, before the axis sign is folded into `angle`.
    pub angle_raw: f32,
    /// `Unique ID` (col 14) — the client's own stable identity for this placement, distinct from
    /// the per-zone row `id`. Populated on 19,366 of 19,367 region-1 rows.
    pub unique_id: u32,
    /// `Collide` (col 8) — per-INSTANCE collision size, overriding the palette's radius.
    pub collide: f32,
    /// `Radius` (col 9) — per-instance radius; the palette entry is only a default.
    pub instance_radius: f32,
    /// `Animate` (col 10) — this placement runs its model's animation.
    pub animate: bool,
    /// `Cave` (col 13) — set on exactly ONE placement in the whole client. Kept anyway: the cost is
    /// a bool, and "a column that is almost always zero" is precisely the shape of the data we keep
    /// discovering the hard way.
    pub cave: bool,
}

/// Parse a zone's `csvNNN.mpk` into its placed fixtures (palette radius pre-joined).
pub fn fixtures_from_csv_mpk(csv_mpk: &[u8]) -> io::Result<Vec<Fixture>> {
    let members = crate::read(csv_mpk)?;
    let find = |name: &str| {
        members
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(name))
            .map(|m| String::from_utf8_lossy(&m.data).into_owned())
            .unwrap_or_default()
    };
    let nifs = parse_nifs(&find("nifs.csv"));
    Ok(parse_fixtures(&find("fixtures.csv"), &nifs))
}

/// `nifs.csv`: two header lines, then `NIF,Textual Name,Filename,…,Radius(col 13),…`.
fn parse_nifs(text: &str) -> HashMap<u32, NifDef> {
    let mut out = HashMap::new();
    for line in text.lines().skip(2) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 14 {
            continue;
        }
        let Ok(id) = f[0].trim().parse::<u32>() else {
            continue;
        };
        out.insert(
            id,
            NifDef {
                name: f[1].trim().to_string(),
                filename: f[2].trim().to_string(),
                radius: f[13].trim().parse().unwrap_or(0.0),
            },
        );
    }
    out
}

/// `fixtures.csv`: two header lines, then
/// `ID,NIF #,Textual Name,X,Y,Z,A,Scale,Collide,Radius,Animate,Ground,…`.
fn parse_fixtures(text: &str, nifs: &HashMap<u32, NifDef>) -> Vec<Fixture> {
    let mut out = Vec::new();
    for line in text.lines().skip(2) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 12 {
            continue;
        }
        let Ok(nif_id) = f[1].trim().parse::<u32>() else {
            continue;
        };
        let id = f[0].trim().parse::<u32>().unwrap_or(0);
        let num = |i: usize| f[i].trim().parse::<f32>().unwrap_or(0.0);
        // Rotation: two CSV formats coexist across zones, with DIFFERENT angle conventions.
        // Both produce a RENDER-frame yaw. Render space mirrors world Y (world +Y = south,
        // render +Y = north, models are authored y-north) — a mirror flips rotation sense, so
        // world-frame angles NEGATE on the way in. The old un-negated values were fit against
        // the doubly-mirrored render and needed the (A-180) fudge + hand fixes to half-work.
        //  * LONG format (e.g. Camelot Hills, 19 cols): a pre-resolved 3D Angle (radians) +
        //    a full rotation axis in cols 15..18. The axis is USUALLY ±Z but not always — 2,559
        //    region-1 rows author a real pitch/roll — so the full axis is kept, not just its sign.
        //  * SHORT format (e.g. Forest Sauvage / Snowdonia, 15 cols): no 3D columns; the `A`
        //    column (col 6) is a raw DAoC HEADING in degrees (0 = north, clockwise).
        //
        // The two branches MUST agree, and until now they did not. `A` is present in BOTH formats,
        // and across region 1 12,027 of the 12,127 rows carrying both encodings satisfy
        //     A = -(3DAngle * sign(axisZ))
        // so the LONG branch above resolves to exactly `+A`. The SHORT branch resolved to `-A`,
        // meaning one authored rotation rendered MIRRORED depending only on which CSV layout its
        // zone happened to use — 4,870 region-1 fixtures. A mirror leaves 0 and 180 fixed, which is
        // why every north/south-facing spot check passed while everything at an angle was backwards.
        // The project invariant is `yaw = +heading` (mesh forward is render -Y), so SHORT drops its
        // negation. `caer crosscheck` asserts the two branches agree and exits 1 if they diverge.
        // The authored axis, normalised. SHORT rows carry no 3D block; they are yaw-only, and the
        // axis we synthesise has to be the one that reproduces `+A` through the same render-frame
        // rule the LONG branch uses. The client's own relation is `A = -(angle * sign(axisZ))`, so
        // `A` corresponds to axis (0,0,-1) with magnitude `A` — NOT (0,0,+1), which would yield -A
        // and resurrect the very branch-sign bug crosscheck exists to catch.
        let axis = if f.len() > 18 {
            let (ax, ay, az) = (num(16), num(17), num(18));
            let len = (ax * ax + ay * ay + az * az).sqrt();
            if len > 1e-6 {
                [ax / len, ay / len, az / len]
            } else {
                [0.0, 0.0, -1.0]
            }
        } else {
            [0.0, 0.0, -1.0]
        };
        let angle_raw = if f.len() > 18 {
            num(15)
        } else {
            num(6).to_radians()
        };

        // The render-frame YAW component of the authored rotation.
        //
        // Reading `-(angle * sign(axisZ))` was only right when the axis IS ±Z. For the 118 region-1
        // rows authored about a pure X/Y axis it reported the tilt magnitude as though it were a
        // heading — and the client agrees those rows have no heading at all, writing `A` = 0. So
        // build the render-frame rotation properly (the world→render mirror maps axis `a` to
        // `(-a.x, a.y, -a.z)`) and extract the yaw from it. For a ±Z axis this reduces exactly to
        // the old expression, so the 11,938 ordinary fixtures are bit-identical.
        let (rx, ry, rz) = (-axis[0], axis[1], -axis[2]);
        let (sh, ch) = (angle_raw * 0.5).sin_cos();
        let (qx, qy, qz, qw) = (rx * sh, ry * sh, rz * sh, ch);
        let angle = (2.0 * (qw * qz + qx * qy)).atan2(1.0 - 2.0 * (qy * qy + qz * qz));

        out.push(Fixture {
            id,
            nif_id,
            name: f[2].trim().to_string(),
            x: num(3),
            y: num(4),
            z: num(5),
            scale: num(7),
            on_ground: f[11].trim() == "1",
            radius: nifs.get(&nif_id).map_or(0.0, |n| n.radius),
            filename: nifs
                .get(&nif_id)
                .map_or_else(String::new, |n| n.filename.clone()),
            angle,
            axis,
            angle_raw,
            unique_id: f.get(14).and_then(|s| s.trim().parse().ok()).unwrap_or(0),
            collide: num(8),
            instance_radius: num(9),
            animate: f.get(10).is_some_and(|s| s.trim() == "1"),
            cave: f.get(13).is_some_and(|s| s.trim() == "1"),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_palette_radius_onto_fixtures() {
        let nifs =
            "h1\nh2\n401,Elm 2,elm2.nif,1,1,65280,0,0,1,0,360,200,400,12,0,0,0,0,0,0,0,0,0\n";
        let fixtures =
            "h1\nh2\n1,401,Elm 2,60928.00,13312.00,1800.00,6,379,0,12,0,1,0,0,1,0.1,0,0,-1\n";
        let palette = parse_nifs(nifs);
        let fx = parse_fixtures(fixtures, &palette);
        assert_eq!(fx.len(), 1);
        let f = &fx[0];
        assert_eq!(f.nif_id, 401);
        assert_eq!((f.x, f.y, f.z), (60928.0, 13312.0, 1800.0));
        assert_eq!(f.scale, 379.0);
        assert!(f.on_ground);
        assert_eq!(f.radius, 12.0);
    }

    /// The full authored rotation axis survives parsing, not just its Z sign.
    ///
    /// The decoder historically read only `sign(axisZ)` on the commented assumption that "fixtures
    /// only yaw". That is false for 2,493 region-1 rows which author a real pitch/roll. This asserts
    /// the axis is retained and unit-normalised so the renderer is *able* to honour it.
    #[test]
    fn retains_the_full_rotation_axis_not_just_its_z_sign() {
        let nifs = "h1\nh2\n401,Stone,stone1.nif,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0\n";
        // A leaning stone: rotation about +X, which is a pitch, not a yaw.
        let long = "h1\nh2\n1,401,Stone,10.0,20.0,30.0,0,100,0,0,0,1,0,0,77,0.9500,1.0,0.0,0.0\n";
        let fx = parse_fixtures(long, &parse_nifs(nifs));
        assert_eq!(fx.len(), 1);
        let f = &fx[0];
        assert!((f.axis[0] - 1.0).abs() < 1e-4, "axis X lost: {:?}", f.axis);
        assert!(
            f.axis[2].abs() < 1e-4,
            "axis Z should be 0 here: {:?}",
            f.axis
        );
        assert!(
            (f.angle_raw - 0.95).abs() < 1e-4,
            "raw angle lost: {}",
            f.angle_raw
        );
        assert_eq!(f.unique_id, 77, "Unique ID (col 14) lost");
    }

    /// The SHORT and LONG layouts encode the same rotation and must decode to the same yaw.
    ///
    /// This is the regression test for the branch-sign bug: SHORT resolved to `-A` while LONG
    /// resolved to `+A`, so a zone's fixtures rendered mirrored purely on which layout it used.
    /// The rows below are the same heading (A = 148) written both ways — the LONG one uses
    /// `A = -(3DAngle * sign(axisZ))`, the relation the client's own data satisfies on 12,027 of
    /// 12,127 region-1 rows. Break either branch and this fails; it does not restate the maths.
    #[test]
    fn short_and_long_layouts_agree_on_the_same_heading() {
        let nifs = "h1\nh2\n401,Tavern,btavern.nif,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0\n";
        // 15 columns, no 3D block: heading lives in col 6.
        let short = "h1\nh2\n1,401,Tavern,100.0,200.0,300.0,148,100,0,0,0,1,0,0,1\n";
        // 19 columns: 3D Angle = 148 deg in radians about -Z, which is A = 148.
        let long = format!(
            "h1\nh2\n1,401,Tavern,100.0,200.0,300.0,148,100,0,0,0,1,0,0,1,{},0.0,0.0,-1.0\n",
            148.0_f32.to_radians()
        );
        let palette = parse_nifs(nifs);
        let a = parse_fixtures(short, &palette);
        let b = parse_fixtures(&long, &palette);
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        let diff = (a[0].angle - b[0].angle).abs();
        assert!(
            diff < 1e-3,
            "SHORT decoded {:.3} rad but LONG decoded {:.3} rad for the same heading",
            a[0].angle,
            b[0].angle
        );
    }
}
