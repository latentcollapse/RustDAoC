//! The character-screen scene loads with real, textured geometry for every realm.
//!
//! **Fails** when the retail tree is absent (REQ-025). Set `CAER_CLIENT`.

use std::path::PathBuf;

fn root() -> PathBuf {
    let r = caer_assets::client_dep::required_caer_client_root("preworld_scene_load");
    assert!(
        r.join("pregame").is_dir(),
        "CAER_CLIENT has no pregame directory: {} — the pre-world scenes need real assets. REQ-025: \
         a test that cannot run must not report pass.",
        r.display()
    );
    r
}

#[test]
fn every_realm_scene_loads_with_textured_geometry() {
    let root = root();
    for (realm, name, min_verts) in [
        (1u8, "Albion", 10_000usize),
        (2, "Midgard", 500),
        (3, "Hibernia", 10_000),
    ] {
        let scene = caer_render::preworld_scene::load(&root, realm)
            .unwrap_or_else(|| panic!("{name} scene must load"));
        let verts: usize = scene.models.iter().map(|m| m.vertices.len()).sum();
        assert!(
            verts > min_verts,
            "{name}: expected real geometry, got {verts} verts"
        );
        assert!(
            !scene.textures.is_empty(),
            "{name}: scene resolved no textures at all — it would render untextured"
        );
        // Bounds must be a real volume, or the camera framing has nothing to aim at.
        let extent: Vec<f32> = (0..3)
            .map(|a| scene.bound_max[a] - scene.bound_min[a])
            .collect();
        assert!(
            extent.iter().all(|e| *e > 1.0),
            "{name}: degenerate bounds {:?}..{:?}",
            scene.bound_min,
            scene.bound_max
        );
        // The framing must sit INSIDE the backdrop dome, above the stage floor, looking across it.
        // The first version of this asserted the opposite — eye outside the bounds — which is how
        // the screen came to show the dome's back face.
        let (eye, focus) = caer_render::preworld_scene::framing(&scene, 16.0 / 9.0);
        let dome = scene.bound_max[0]
            .max(scene.bound_max[1])
            .max(-scene.bound_min[0])
            .max(-scene.bound_min[1]);
        let eye_r = (eye.x * eye.x + eye.y * eye.y).sqrt();
        assert!(
            eye_r < dome,
            "{name}: eye radius {eye_r:.0} is outside the backdrop dome {dome:.0} — that renders \
             the dome's back face, not the scene"
        );
        assert!(
            eye.z > scene.ground_z,
            "{name}: eye is below the stage floor"
        );
        // The authored backdrop must be BEHIND the character, not behind the camera.
        //
        // This used to aim at `stage_center`, a percentile centroid of "content". That quantity is
        // dominated by whatever the scene has most of — Hibernia's canopy puts it at (273,150)
        // while every named landmark in that scene measures between -117° and -164° of bearing
        // from the anchor — so it pointed the camera away from the portal and the tower and still
        // passed. The realm banner is authored, is present in all three scenes, and is part of the
        // backdrop the player is supposed to see, so it is the thing to test against.
        let look = glam::Vec3::new(focus.x - eye.x, focus.y - eye.y, 0.0);
        let banner = scene
            .models
            .iter()
            .flat_map(|m| m.vertices.iter())
            .filter(|v| v.pos.iter().all(|c| c.is_finite()))
            .fold((glam::Vec3::ZERO, 0usize), |(sum, n), v| {
                // The banner cluster sits within ~400u of the anchor in every realm.
                let d = glam::Vec3::new(
                    v.pos[0] - scene.subject_anchor.x,
                    v.pos[1] - scene.subject_anchor.y,
                    0.0,
                );
                if d.length() < 400.0 && v.pos[2] > scene.subject_anchor.z + 60.0 {
                    (sum + glam::Vec3::new(v.pos[0], v.pos[1], 0.0), n + 1)
                } else {
                    (sum, n)
                }
            });
        assert!(
            banner.1 > 0,
            "{name}: no backdrop geometry near the anchor to frame against"
        );
        let backdrop = banner.0 / banner.1 as f32;
        let to_backdrop = glam::Vec3::new(backdrop.x - eye.x, backdrop.y - eye.y, 0.0);
        assert!(
            to_backdrop.length() > 1.0 && look.normalize().dot(to_backdrop.normalize()) > 0.5,
            "{name}: the authored backdrop is behind the camera — eye ({:.0},{:.0}) look \
             ({:.2},{:.2}) backdrop ({:.0},{:.0})",
            eye.x,
            eye.y,
            look.normalize().x,
            look.normalize().y,
            backdrop.x,
            backdrop.y
        );
        assert!(focus.is_finite(), "{name}: non-finite focus");
        println!(
            "{name}: {verts} verts, {} textures, extent {:?}",
            scene.textures.len(),
            extent
        );
    }
}

/// Every authored control stays fully inside the window at any reasonable size.
///
/// The creation form's bottom row is authored at y=752 of a 768-tall space — 16px from the edge —
/// and a playtest reported Cancel and Continue "not showing up". They were not clipped: in that
/// build they were unlabelled bare text on dark stone with no button art. This pins the geometry
/// anyway, because a control drawn outside the window is the other way that report comes true.
#[test]
fn authored_controls_stay_inside_the_window() {
    use caer_render::preworld::{
        charcreate_continue_click_point, charselect_slot_rect, hit_charcreate, PreWorldAction,
    };
    for vp in [
        (1024.0, 768.0),
        (1150.0, 850.0),
        (800.0, 600.0),
        (1280.0, 720.0),
        (1600.0, 1000.0),
        (2560.0, 1440.0),
        (1024.0, 1024.0),
    ] {
        let (vw, vh) = vp;
        // Continue must be inside, and must still hit-test as Continue where it is drawn.
        let (cx, cy) = charcreate_continue_click_point(vp);
        assert!(
            cx > 0.0 && cx < vw && cy > 0.0 && cy < vh,
            "{vp:?}: Continue click point ({cx:.0},{cy:.0}) is outside the window"
        );
        assert_eq!(
            hit_charcreate(cx, cy, vp),
            Some(PreWorldAction::CharCreateContinue),
            "{vp:?}: the Continue point does not hit Continue"
        );
        // Every character slot row, top and bottom.
        for slot in 0..10u8 {
            let r = charselect_slot_rect(slot, vp);
            assert!(
                r.x >= 0.0 && r.y >= 0.0 && r.x + r.w <= vw && r.y + r.h <= vh,
                "{vp:?}: slot {slot} row {r:?} leaves the window"
            );
        }
    }
}

/// Structural oracle: every realm's texture bindings and part geometry, against a committed
/// fingerprint.
///
/// The defect this exists for was invisible to every other check — the suite was green while
/// Midgard's ground wore a skull prop atlas, Albion's grass wore rock, and Hibernia's trunks wore
/// moss. Nothing compared what we bind against what we bound last time, so only a human looking at
/// the screen ever caught it.
///
/// Regenerate deliberately with `CAER_BLESS_SCENE_FINGERPRINT=1` and review the diff as a
/// behaviour change. A silent regeneration defeats the whole point.
#[test]
fn realm_scene_fingerprints_match_the_golden() {
    let root = root();
    let mut actual = String::new();
    for (realm, name) in [(1u8, "Albion"), (2, "Midgard"), (3, "Hibernia")] {
        let path = root.join(caer_render::preworld_scene::scene_archive(realm));
        let Ok(Some(nif)) =
            caer_assets::open_first(&path, |n| n.to_ascii_lowercase().ends_with(".nif"))
        else {
            panic!("{name}: no scene NIF at {}", path.display());
        };
        let model = caer_assets::nif::read_model(&nif.data).expect("scene parses");
        let mut rows: Vec<String> = model
            .parts
            .iter()
            .filter(|p| !p.positions.is_empty())
            .map(|p| {
                let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
                for v in &p.positions {
                    if !v.iter().all(|c| c.is_finite()) {
                        continue;
                    }
                    for a in 0..3 {
                        lo[a] = lo[a].min(v[a]);
                        hi[a] = hi[a].max(v[a]);
                    }
                }
                // Extents rounded to 10 units: identity of the part, not float noise.
                format!(
                    "{name}\t{}\t{}\t{}\t{:.0}x{:.0}x{:.0}",
                    p.name,
                    p.texture.as_deref().unwrap_or("-"),
                    p.positions.len(),
                    ((hi[0] - lo[0]) / 10.0).round() * 10.0,
                    ((hi[1] - lo[1]) / 10.0).round() * 10.0,
                    ((hi[2] - lo[2]) / 10.0).round() * 10.0,
                )
            })
            .collect();
        rows.sort();
        for r in rows {
            actual.push_str(&r);
            actual.push('\n');
        }
    }

    let golden =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/preworld_scene_bindings.txt");
    if std::env::var_os("CAER_BLESS_SCENE_FINGERPRINT").is_some() {
        std::fs::write(&golden, &actual).expect("write fingerprint");
        println!("blessed {}", golden.display());
        return;
    }
    let expected = std::fs::read_to_string(&golden).unwrap_or_else(|e| {
        panic!(
            "no scene fingerprint at {} ({e}). Generate with CAER_BLESS_SCENE_FINGERPRINT=1 and \
             review it before committing.",
            golden.display()
        )
    });
    if expected != actual {
        let diff: Vec<String> = expected
            .lines()
            .zip(actual.lines())
            .filter(|(a, b)| a != b)
            .take(10)
            .map(|(a, b)| format!("  golden: {a}\n  actual: {b}"))
            .collect();
        panic!(
            "realm scene bindings changed ({} golden lines vs {} actual).\n{}\n\nIf intended, \
             re-bless with CAER_BLESS_SCENE_FINGERPRINT=1 and review the diff.",
            expected.lines().count(),
            actual.lines().count(),
            diff.join("\n")
        );
    }
}
