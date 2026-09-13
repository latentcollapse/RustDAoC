//! What each realm scene actually contains — node names, part names, measured geometry.
//!
//! Diagnostic, not a gate. Run with `--nocapture`.
//!
//! Exists because the character-screen camera is a provenance gap that three separate geometric
//! fits failed to close, each in a way that only a render showed. Before guessing a fourth time:
//! ask the asset what it is called.

mod common;

use common::root;

#[test]
fn report_scene_node_names() {
    let root = root();
    for (realm, name) in [(1u8, "Albion"), (2, "Midgard"), (3, "Hibernia")] {
        let path = root.join(caer_render::preworld_scene::scene_archive(realm));

        // EVERY member first. `preworld_scene::load` calls `open_first`, so if an archive ships
        // more than one NIF the scene is only ever as complete as whichever one comes back first
        // — which is exactly the shape of "the Hibernian portal and tower are missing".
        match caer_assets::list_names(&path) {
            Ok(names) => {
                let nifs: Vec<&String> = names
                    .iter()
                    .filter(|n| n.to_ascii_lowercase().ends_with(".nif"))
                    .collect();
                println!(
                    "\n=== {name} archive: {} member(s), {} NIF(s) ===",
                    names.len(),
                    nifs.len()
                );
                for n in &names {
                    println!("    member {n}");
                }
                if nifs.len() > 1 {
                    println!(
                        "  !! {} NIFs present but load() renders only the first — {} are dropped",
                        nifs.len(),
                        nifs.len() - 1
                    );
                }
            }
            Err(e) => println!("{name}: cannot list {}: {e}", path.display()),
        }

        let Ok(Some(nif)) =
            caer_assets::open_first(&path, |n| n.to_ascii_lowercase().ends_with(".nif"))
        else {
            println!("{name}: no NIF");
            continue;
        };
        println!("=== {name} ({}) ===", nif.name);
        match caer_assets::nif::read_skeleton(&nif.data) {
            Ok(sk) => {
                println!("  {} nodes", sk.bones.len());

                // The named markers, in WORLD space with their orientation. `local` alone is
                // meaningless for placing anything — it is relative to a parent chain — and the
                // subject anchor is the one number this whole screen is composed around.
                println!("  -- named markers (world transform) --");
                for (i, b) in sk.bones.iter().enumerate() {
                    let n = b.name.to_ascii_lowercase();
                    let interesting = n.contains("snapshot")
                        || n.contains("collidee")
                        || n == "visible"
                        || n == "root"
                        || n.contains("camera")
                        || n.contains("char")
                        || n.contains("player")
                        || n.contains("tower")
                        || n.contains("portal");
                    if !interesting {
                        continue;
                    }
                    let w = caer_assets::nif::xform_to_mat4(&b.world_bind);
                    let parent = b
                        .parent
                        .and_then(|p| sk.bones.get(p))
                        .map_or("-", |p| p.name.as_str());
                    // Column 1 of the basis is the node's local +Y, i.e. which way it "faces" in
                    // the client's Z-up authoring space.
                    println!(
                        "    [{i:>3}] {:<22} parent {:<18} world ({:>7.0},{:>7.0},{:>7.0})  \
                         +Y ({:>5.2},{:>5.2},{:>5.2})  +Z ({:>5.2},{:>5.2},{:>5.2})",
                        b.name,
                        parent,
                        w[3][0],
                        w[3][1],
                        w[3][2],
                        w[1][0],
                        w[1][1],
                        w[1][2],
                        w[2][0],
                        w[2][1],
                        w[2][2],
                    );
                }

                println!("  -- all nodes (local) --");
                for b in &sk.bones {
                    let t = caer_assets::nif::xform_to_mat4(&b.local);
                    println!(
                        "    node {:?} local ({:.0},{:.0},{:.0})",
                        b.name, t[3][0], t[3][1], t[3][2]
                    );
                }
            }
            Err(e) => println!("  skeleton walk failed: {e}"),
        }
        if let Ok(model) = caer_assets::nif::read_model(&nif.data) {
            println!("  {} mesh parts", model.parts.len());
            for p in &model.parts {
                println!(
                    "    part {:?} tex {:?} alpha {:?} diffuse {:?} verts {}",
                    p.name,
                    p.texture.as_deref().unwrap_or("-"),
                    p.alpha,
                    p.diffuse,
                    p.positions.len()
                );
            }
        }
    }
}

/// Is `flag_snapshot01` the character's feet, or the camera?
///
/// Both readings fit the name, and picking wrong puts the body where the lens belongs. The ground
/// decides it: a stand point sits ON the floor under its own XY, a camera sits well above it. This
/// measures the scene's own surface directly beneath the marker and compares.
///
/// It also prints what the current composition uses — origin `(0,0,origin_floor_z)` — so the size
/// of the error is a number rather than an impression.
#[test]
fn snapshot_marker_versus_ground() {
    let root = root();
    for (realm, name) in [(1u8, "Albion"), (2, "Midgard"), (3, "Hibernia")] {
        let path = root.join(caer_render::preworld_scene::scene_archive(realm));
        let Ok(Some(nif)) =
            caer_assets::open_first(&path, |n| n.to_ascii_lowercase().ends_with(".nif"))
        else {
            continue;
        };
        let Ok(sk) = caer_assets::nif::read_skeleton(&nif.data) else {
            continue;
        };
        let Some(marker) = sk
            .bones
            .iter()
            .find(|b| b.name.eq_ignore_ascii_case("flag_snapshot01"))
        else {
            println!("{name}: no flag_snapshot01");
            continue;
        };
        let m = caer_assets::nif::xform_to_mat4(&marker.world_bind);
        let (mx, my, mz) = (m[3][0], m[3][1], m[3][2]);

        let Some(scene) = caer_render::preworld_scene::load(&root, realm) else {
            continue;
        };

        // Surface height under an XY: the highest vertex within a small radius. Highest, not
        // mean — a character stands on the top face, and the mean would be dragged down by the
        // underside of whatever it is standing on.
        let height_under = |x: f32, y: f32, r: f32| -> Option<(f32, usize)> {
            let mut best: Option<f32> = None;
            let mut n = 0usize;
            for b in &scene.models {
                for v in &b.vertices {
                    let (dx, dy) = (v.pos[0] - x, v.pos[1] - y);
                    if dx * dx + dy * dy <= r * r && v.pos[2].is_finite() {
                        n += 1;
                        best = Some(best.map_or(v.pos[2], |c: f32| c.max(v.pos[2])));
                    }
                }
            }
            best.map(|h| (h, n))
        };

        let _ = height_under(0.0, 0.0, 1.0);

        // Z distribution under an XY. A stand point wants the FLOOR, and the floor is not the
        // max (that is whatever prop is nearby) nor the min (that is the underside). Report the
        // spread so the shape of the surface is visible instead of assumed.
        let profile = |x: f32, y: f32, r: f32| -> Option<(f32, f32, f32, usize)> {
            let mut zs: Vec<f32> = Vec::new();
            for b in &scene.models {
                for v in &b.vertices {
                    let (dx, dy) = (v.pos[0] - x, v.pos[1] - y);
                    if dx * dx + dy * dy <= r * r && v.pos[2].is_finite() {
                        zs.push(v.pos[2]);
                    }
                }
            }
            if zs.is_empty() {
                return None;
            }
            zs.sort_by(f32::total_cmp);
            let n = zs.len();
            Some((zs[0], zs[n / 2], zs[n - 1], n))
        };

        // Every candidate anchor the assets actually offer, measured the same way.
        let mut candidates: Vec<(String, f32, f32, f32)> =
            vec![("origin (current)".into(), 0.0, 0.0, scene.origin_floor_z)];
        candidates.push(("flag_snapshot01".into(), mx, my, mz));
        for (i, b) in sk.bones.iter().enumerate() {
            let n = b.name.to_ascii_lowercase();
            if n == "collidee" || n == "visible" {
                let w = caer_assets::nif::xform_to_mat4(&b.world_bind);
                candidates.push((format!("{} [{i}]", b.name), w[3][0], w[3][1], w[3][2]));
            }
        }

        println!("\n=== {name} ===");
        println!(
            "  {:<22} {:>7} {:>7} {:>7} | {:>8} {:>8} {:>8} {:>6} | {:>8}",
            "candidate", "x", "y", "z", "min z", "med z", "max z", "verts", "z-med"
        );
        for (label, x, y, z) in &candidates {
            match profile(*x, *y, 60.0) {
                Some((lo, med, hi, n)) => println!(
                    "  {label:<22} {x:>7.0} {y:>7.0} {z:>7.0} | {lo:>8.0} {med:>8.0} {hi:>8.0} {n:>6} | {:>8.0}",
                    z - med
                ),
                None => println!(
                    "  {label:<22} {x:>7.0} {y:>7.0} {z:>7.0} | {:>8} no geometry within 60u",
                    ""
                ),
            }
        }
    }
}

/// Where the landmarks are, relative to the stand point.
///
/// The tower and the portal are in the Hibernian NIF and decode fine, so "missing" means out of
/// frame. Which frame is right is a bearing question, and the bearing is answerable: measure where
/// the named backdrop geometry sits relative to the anchor, and the camera belongs opposite it.
#[test]
fn landmark_bearings_from_the_stand_point() {
    let root = root();
    // Textures that name a landmark rather than ground cover.
    let landmark = |t: &str| {
        let t = t.to_ascii_lowercase();
        t.contains("tower")
            || t.contains("portal")
            || t.contains("arch")
            || t.contains("banner")
            || t.contains("keep")
            || t.contains("glow")
            || t.contains("marble")
            || t.contains("trim")
            || t.contains("filigree")
    };

    for (realm, name) in [(1u8, "Albion"), (2, "Midgard"), (3, "Hibernia")] {
        let path = root.join(caer_render::preworld_scene::scene_archive(realm));
        let Ok(Some(nif)) =
            caer_assets::open_first(&path, |n| n.to_ascii_lowercase().ends_with(".nif"))
        else {
            continue;
        };
        let Ok(sk) = caer_assets::nif::read_skeleton(&nif.data) else {
            continue;
        };
        let Some(anchor) = sk
            .bones
            .iter()
            .find(|b| b.name.eq_ignore_ascii_case("collidee"))
            .map(|b| caer_assets::nif::xform_to_mat4(&b.world_bind))
        else {
            continue;
        };
        let (ax, ay) = (anchor[3][0], anchor[3][1]);
        let Ok(model) = caer_assets::nif::read_model(&nif.data) else {
            continue;
        };

        println!("\n=== {name} — anchor ({ax:.0},{ay:.0}) ===");
        let mut rows: Vec<(String, f32, f32, f32, f32, usize)> = Vec::new();
        for p in &model.parts {
            let Some(tex) = p.texture.as_deref() else {
                continue;
            };
            if !landmark(tex) || p.positions.is_empty() {
                continue;
            }
            let n = p.positions.len() as f32;
            let (cx, cy, cz) = p.positions.iter().fold((0.0, 0.0, 0.0), |a, v| {
                (a.0 + v[0] / n, a.1 + v[1] / n, a.2 + v[2] / n)
            });
            // Bearing in degrees from +Y, measured about +Z. 0 = straight "north" of the anchor.
            let (dx, dy) = (cx - ax, cy - ay);
            let bearing = dx.atan2(dy).to_degrees();
            let dist = (dx * dx + dy * dy).sqrt();
            rows.push((tex.to_string(), cx, cy, cz, bearing, p.positions.len()));
            let _ = dist;
        }
        rows.sort_by(|a, b| b.5.cmp(&a.5));
        println!(
            "  {:<28} {:>7} {:>7} {:>7} {:>9} {:>8} {:>7}",
            "texture", "cx", "cy", "cz", "bearing", "dist", "verts"
        );
        for (tex, cx, cy, cz, bearing, verts) in rows.iter().take(12) {
            let (dx, dy) = (cx - ax, cy - ay);
            println!(
                "  {tex:<28} {cx:>7.0} {cy:>7.0} {cz:>7.0} {bearing:>8.0}° {:>8.0} {verts:>7}",
                (dx * dx + dy * dy).sqrt()
            );
        }
        println!("  (bearing 0° = +Y from the anchor; the camera currently sits at -Y looking +Y)");
    }
}

/// Does the asset carry a camera, or is the framing genuinely ours to derive?
///
/// The module doc asserts "no `NiCamera` exists in any of the three scenes" and everything
/// downstream has been built on that claim. It came from a previous pass, not from a check that
/// ran, so this prints every distinct block type per scene and lets the file answer.
#[test]
fn scene_block_types() {
    let root = root();
    for (realm, name) in [(1u8, "Albion"), (2, "Midgard"), (3, "Hibernia")] {
        let path = root.join(caer_render::preworld_scene::scene_archive(realm));
        let Ok(Some(nif)) =
            caer_assets::open_first(&path, |n| n.to_ascii_lowercase().ends_with(".nif"))
        else {
            continue;
        };
        match caer_assets::nif::read_header(&nif.data) {
            Ok(h) => {
                use std::collections::BTreeMap;
                let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
                for t in &h.block_types {
                    *counts.entry(t.as_str()).or_default() += 1;
                }
                println!("\n=== {name}: {} block types ===", counts.len());
                for (t, n) in &counts {
                    let flag = if t.to_ascii_lowercase().contains("camera")
                        || t.to_ascii_lowercase().contains("light")
                    {
                        "  <-- VIEWPOINT/LIGHT DATA"
                    } else {
                        ""
                    };
                    println!("  {n:>5}  {t}{flag}");
                }
            }
            Err(e) => println!("{name}: header parse failed: {e}"),
        }
    }
}

/// The camera bearing the assets imply, against the composition retail actually ships.
///
/// Retail frames all three realms the same way: realm banner on the left, character centred,
/// tower on the right. That only happens when the view axis BISECTS the two flanking landmarks —
/// looking down 180° as we do puts both on the same side, which is why the banner sits beside the
/// character and the tower falls out of frame.
///
/// Landmarks are gathered by node name (Albion literally names its `tower` nodes) and by texture
/// name (Hibernia's tower is `H_elftower2_*`), because neither source alone covers all three.
#[test]
fn camera_bearing_implied_by_landmarks() {
    let root = root();
    for (realm, name) in [(1u8, "Albion"), (2, "Midgard"), (3, "Hibernia")] {
        let path = root.join(caer_render::preworld_scene::scene_archive(realm));
        let Ok(Some(nif)) =
            caer_assets::open_first(&path, |n| n.to_ascii_lowercase().ends_with(".nif"))
        else {
            continue;
        };
        let Ok(sk) = caer_assets::nif::read_skeleton(&nif.data) else {
            continue;
        };
        let Some(anchor) = sk
            .bones
            .iter()
            .find(|b| b.name.eq_ignore_ascii_case("collidee"))
            .map(|b| caer_assets::nif::xform_to_mat4(&b.world_bind))
        else {
            continue;
        };
        let (ax, ay) = (anchor[3][0], anchor[3][1]);
        let bearing = |x: f32, y: f32| (x - ax).atan2(y - ay).to_degrees();

        println!("\n=== {name} — anchor ({ax:.0},{ay:.0}) ===");
        let mut marks: Vec<(String, f32, f32, usize)> = Vec::new();

        // Node-named landmarks (Albion's towers).
        for b in &sk.bones {
            let n = b.name.to_ascii_lowercase();
            if n.contains("tower") || n.contains("portal") {
                let w = caer_assets::nif::xform_to_mat4(&b.world_bind);
                if w[3][0].is_finite() && w[3][1].is_finite() {
                    let d = ((w[3][0] - ax).powi(2) + (w[3][1] - ay).powi(2)).sqrt();
                    marks.push((format!("node {}", b.name), bearing(w[3][0], w[3][1]), d, 0));
                }
            }
        }
        // Texture-named landmarks.
        if let Ok(model) = caer_assets::nif::read_model(&nif.data) {
            for p in &model.parts {
                let Some(tex) = p.texture.as_deref() else {
                    continue;
                };
                let t = tex.to_ascii_lowercase();
                let is_mark = t.contains("banner")
                    || t.contains("tower")
                    || t.contains("portal")
                    || t.contains("elfshop");
                if !is_mark || p.positions.is_empty() || p.positions.len() < 100 {
                    continue;
                }
                let n = p.positions.len() as f32;
                let (cx, cy) = p
                    .positions
                    .iter()
                    .fold((0.0, 0.0), |a, v| (a.0 + v[0] / n, a.1 + v[1] / n));
                if !cx.is_finite() || !cy.is_finite() {
                    continue;
                }
                let d = ((cx - ax).powi(2) + (cy - ay).powi(2)).sqrt();
                marks.push((tex.to_string(), bearing(cx, cy), d, p.positions.len()));
            }
        }
        // The named-texture filter above only sees props we already guessed the names of. Midgard's
        // span came out 135 deg wide because two DISTANT background banners matched while its real
        // near-field structure did not, so also report the heaviest geometry regardless of name —
        // that is what actually fills the frame.
        if let Ok(model) = caer_assets::nif::read_model(&nif.data) {
            let mut heavy: Vec<(usize, String, f32, f32)> = Vec::new();
            for p in &model.parts {
                if p.positions.len() < 400 {
                    continue;
                }
                let n = p.positions.len() as f32;
                let (cx, cy) = p
                    .positions
                    .iter()
                    .fold((0.0, 0.0), |a, v| (a.0 + v[0] / n, a.1 + v[1] / n));
                if !cx.is_finite() || !cy.is_finite() {
                    continue;
                }
                let d = ((cx - ax).powi(2) + (cy - ay).powi(2)).sqrt();
                heavy.push((
                    p.positions.len(),
                    p.texture.clone().unwrap_or_else(|| "-".into()),
                    bearing(cx, cy),
                    d,
                ));
            }
            heavy.sort_by(|a, b| b.0.cmp(&a.0));
            println!("  -- heaviest geometry (any texture) --");
            for (v, tex, b, d) in heavy.iter().take(10) {
                println!("     {v:>6} verts  {tex:<30} bearing {b:>7.0}  dist {d:>7.0}");
            }
        }

        marks.sort_by(|a, b| a.1.total_cmp(&b.1));
        for (label, b, d, v) in &marks {
            println!("  {label:<30} bearing {b:>7.0}°  dist {d:>7.0}  verts {v:>6}");
        }
        if marks.len() >= 2 {
            let lo = marks.first().unwrap().1;
            let hi = marks.last().unwrap().1;
            println!(
                "  span {lo:.0}° .. {hi:.0}°   BISECTOR {:.0}°   (camera currently looks down 180°)",
                (lo + hi) * 0.5
            );
        }
    }
}

#[test]
fn report_scene_geometry() {
    let root = root();
    for (realm, name) in [(1u8, "Albion"), (2, "Midgard"), (3, "Hibernia")] {
        let Some(s) = caer_render::preworld_scene::load(&root, realm) else {
            println!("{name}: no scene");
            continue;
        };
        let (eye, focus) = caer_render::preworld_scene::framing(&s, 16.0 / 9.0);
        println!(
            "{name}: bounds {:?}..{:?}\n  ground_z {:.0} content_r {:.0} stage_c ({:.0},{:.0},{:.0}) stage_r {:.0} stage_top {:.0}\n  eye ({:.0},{:.0},{:.0}) focus ({:.0},{:.0},{:.0})",
            s.bound_min,
            s.bound_max,
            s.ground_z,
            s.content_radius,
            s.stage_center.x,
            s.stage_center.y,
            s.stage_center.z,
            s.stage_radius,
            s.stage_top,
            eye.x,
            eye.y,
            eye.z,
            focus.x,
            focus.y,
            focus.z,
        );
    }
}

/// Which textures each scene asks for, and where (if anywhere) they can be found.
#[test]
fn report_scene_texture_resolution() {
    let root = root();
    let mut loose: Vec<String> = Vec::new();
    for dir in ["pregame", "pregame/textures"] {
        if let Ok(rd) = std::fs::read_dir(root.join(dir)) {
            loose.extend(
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().to_ascii_lowercase()),
            );
        }
    }
    let mut archived: Vec<String> = Vec::new();
    for arch in [
        "pregame/pregame.mpk",
        "pregame/pregame002.mpk",
        "pregame/pregame003.mpk",
    ] {
        if let Ok(names) = caer_assets::list_names(root.join(arch)) {
            archived.extend(names.into_iter().map(|n| n.to_ascii_lowercase()));
        }
    }
    for (realm, name) in [(1u8, "Albion"), (2, "Midgard"), (3, "Hibernia")] {
        let path = root.join(caer_render::preworld_scene::scene_archive(realm));
        let Ok(Some(nif)) =
            caer_assets::open_first(&path, |n| n.to_ascii_lowercase().ends_with(".nif"))
        else {
            continue;
        };
        let Ok(model) = caer_assets::nif::read_model(&nif.data) else {
            continue;
        };
        let mut want: Vec<String> = model
            .parts
            .iter()
            .filter_map(|p| p.texture.clone())
            .map(|t| t.to_ascii_lowercase())
            .collect();
        want.sort();
        want.dedup();
        println!("=== {name}: {} distinct textures ===", want.len());
        for t in want {
            let base = t.rsplit(['/', '\\']).next().unwrap_or(&t).to_string();
            let stem = base
                .rsplit_once('.')
                .map_or(base.clone(), |(s, _)| s.to_string());
            let where_ = if loose.contains(&base) {
                "loose".to_string()
            } else if archived.contains(&base) {
                "mpk".to_string()
            } else {
                let alt: Vec<&String> = loose
                    .iter()
                    .chain(archived.iter())
                    .filter(|f| {
                        f.starts_with(&stem) || stem.starts_with(f.split('.').next().unwrap_or(""))
                    })
                    .take(3)
                    .collect();
                format!("MISSING (near: {alt:?})")
            };
            println!("  {t} -> {where_}");
        }
    }
}

/// Can the fig3 resolver actually produce a base body? Diagnostic for the character-screen avatar.
#[test]
fn report_avatar_parts() {
    let root = root();
    match caer_assets::figures::FigureModels::load(root.join("gamedata.mpk")) {
        Ok(fm) => {
            for (race, gender, name) in [
                (1u8, 0u8, "Briton male"),
                (1, 1, "Briton female"),
                (6, 0, "Norseman male"),
            ] {
                let parts = fm.base_body(race, gender);
                println!(
                    "{name}: fig {:?}, {} base parts {:?}",
                    fm.figure_id(race, gender),
                    parts.len(),
                    parts.iter().map(|p| p.filename.clone()).collect::<Vec<_>>()
                );
            }
        }
        Err(e) => println!("FigureModels::load failed: {e}"),
    }
}

/// Every part's world-space box, largest first.
///
/// Landmark coordinates for solving the retail camera, which no scene asset carries (see the
/// PROVENANCE GAP on `framing_around_character`). A retail screenshot is the only artefact that
/// has ever contained the answer; this supplies the other half of the correspondence.
#[test]
fn report_part_world_boxes() {
    let root = root();
    for (realm, name) in [(1u8, "Albion"), (2, "Midgard"), (3, "Hibernia")] {
        let path = root.join(caer_render::preworld_scene::scene_archive(realm));
        let Ok(Some(nif)) =
            caer_assets::open_first(&path, |n| n.to_ascii_lowercase().ends_with(".nif"))
        else {
            continue;
        };
        let Ok(model) = caer_assets::nif::read_model(&nif.data) else {
            continue;
        };
        println!(
            "\n=== {name}: {} parts (world space) ===",
            model.parts.len()
        );
        println!(
            "{:<22} {:>6}  {:>26}  {:>26}  {:>8}",
            "part", "verts", "min x,y,z", "max x,y,z", "diag"
        );
        let mut rows: Vec<(f32, String)> = Vec::new();
        for p in &model.parts {
            if p.positions.is_empty() {
                continue;
            }
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
            if lo[0] > hi[0] {
                continue;
            }
            let diag =
                ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2) + (hi[2] - lo[2]).powi(2))
                    .sqrt();
            rows.push((
                diag,
                format!(
                    "{:<22} {:>6}  {:>8.0},{:>8.0},{:>8.0}  {:>8.0},{:>8.0},{:>8.0}  {diag:>8.0}  tex {}",
                    p.name,
                    p.positions.len(),
                    lo[0],
                    lo[1],
                    lo[2],
                    hi[0],
                    hi[1],
                    hi[2],
                    p.texture.as_deref().unwrap_or("-"),
                ),
            ));
        }
        rows.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, r) in rows {
            println!("{r}");
        }
    }
}

/// Slot → filename for every texturing property.
///
/// Map slots 0..6 then the shader-texture list. Shader tex 0 is the diffuse the client actually
/// renders; map slot 0 is a fixed-function fallback the artists left stale on several parts, which
/// is how a prop atlas ended up on Midgard's ground sheet.
#[test]
fn report_texturing_stages() {
    let root = root();
    for (realm, name) in [(1u8, "Albion"), (2, "Midgard"), (3, "Hibernia")] {
        let path = root.join(caer_render::preworld_scene::scene_archive(realm));
        let Ok(Some(nif)) =
            caer_assets::open_first(&path, |n| n.to_ascii_lowercase().ends_with(".nif"))
        else {
            continue;
        };
        let Ok(stages) = caer_assets::nif::texturing_stages(&nif.data) else {
            println!("{name}: cannot read texturing stages");
            continue;
        };
        let multi = stages.iter().filter(|(_, _, s)| s.len() > 1).count();
        println!(
            "\n=== {name}: {} texturing properties, {multi} with more than one stage ===",
            stages.len()
        );
        for (block, map_slots, slots) in &stages {
            let show: Vec<String> = slots
                .iter()
                .map(|(s, f)| {
                    let kind = if s < map_slots { "map" } else { "shader" };
                    format!("{kind} {s} = {f}")
                })
                .collect();
            println!(
                "  block {block:>4} (maps {map_slots})  {}",
                show.join("  |  ")
            );
        }
    }
}

/// Is this part a prop atlas stretched over a ground sheet?
///
/// Pure so the detector can be run against known-bad and known-good inputs directly. It is not a
/// gate until it has been seen firing — the first version of this check used flatness with a 0.06
/// threshold, and the real sheet measures 0.064, so it passed with the defect fully present.
fn atlas_on_a_ground_sheet(texture: &str, extent_x: f32, extent_y: f32) -> bool {
    texture.eq_ignore_ascii_case("skull_snow01.dds") && extent_x.max(extent_y) > 200.0
}

/// The negative control, as a test rather than a thing someone remembers to do by hand.
#[test]
fn the_atlas_detector_fires_on_the_known_bad_and_not_on_the_known_good() {
    // Known-bad: `foregrpound:1` as measured while the defect was live.
    assert!(
        atlas_on_a_ground_sheet("skull_snow01.dds", 2001.0, 2490.0),
        "detector cannot see the defect it exists to catch"
    );
    // Known-good: skull01 and skull03, which legitimately wear the atlas.
    assert!(!atlas_on_a_ground_sheet("skull_snow01.dds", 7.0, 10.0));
    assert!(!atlas_on_a_ground_sheet("skull_snow01.dds", 22.0, 9.0));
    // A sheet is only wrong when it wears the atlas.
    assert!(!atlas_on_a_ground_sheet("Mid_ground02.dds", 2001.0, 2490.0));
}

/// Shape of the meshes wearing `skull_snow01.dds`: prop-sized clusters, or a flat sheet?
///
/// `skull01`/`skull03` are real props and legitimately wear the atlas. A wide flat sheet wearing it
/// means base-texture selection regressed to map slot 0.
#[test]
fn midgard_skull_geometry_shape() {
    let root = root();
    let path = root.join(caer_render::preworld_scene::scene_archive(2));
    let Ok(Some(nif)) =
        caer_assets::open_first(&path, |n| n.to_ascii_lowercase().ends_with(".nif"))
    else {
        return;
    };
    let Ok(model) = caer_assets::nif::read_model(&nif.data) else {
        return;
    };
    let mut sheets_wearing_the_atlas: Vec<String> = Vec::new();
    println!(
        "\n{:<20} {:>6} {:>28} {:>22}",
        "part", "verts", "extent x,y,z", "centre x,y"
    );
    for p in &model.parts {
        let t = p.texture.as_deref().unwrap_or("");
        if !(t.eq_ignore_ascii_case("skull_snow01.dds")
            || t.eq_ignore_ascii_case("Mid_ground02.dds"))
        {
            continue;
        }
        if p.positions.is_empty() {
            continue;
        }
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
        // UV range decides whether a big sheet TILES its texture or stretches one copy across it.
        // A 2000-unit sheet with UVs in 0..1 stretches a 512px image over 2000 units, which is
        // exactly how a skull becomes a smeared continent.
        let (mut ulo, mut uhi) = (f32::MAX, f32::MIN);
        for uv in &p.uvs {
            if uv[0].is_finite() {
                ulo = ulo.min(uv[0]);
                uhi = uhi.max(uv[0]);
            }
        }
        let e = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
        // Flatness: a prop is chunky in all three axes; a smeared sheet is wide and paper-thin.
        let flat = e[2] / e[0].max(e[1]).max(1.0);
        if atlas_on_a_ground_sheet(t, e[0], e[1]) {
            sheets_wearing_the_atlas.push(format!(
                "{} ({:.0}x{:.0} units, flat {flat:.3})",
                p.name, e[0], e[1]
            ));
        }
        println!(
            "{:<20} {:>6} {:>8.0},{:>8.0},{:>8.0} {:>10.0},{:>10.0}  flat {flat:.3}  u {ulo:.1}..{uhi:.1}{}",
            p.name,
            p.positions.len(),
            e[0],
            e[1],
            e[2],
            (lo[0] + hi[0]) * 0.5,
            (lo[1] + hi[1]) * 0.5,
            if flat < 0.06 && e[0].max(e[1]) > 400.0 {
                "  <- WIDE AND FLAT"
            } else {
                ""
            }
        );
    }
    assert!(
        sheets_wearing_the_atlas.is_empty(),
        "skull_snow01.dds is a prop UV atlas, not a tiling ground texture. A wide flat sheet \
         wearing it means base-texture selection regressed to map slot 0 — shader texture 0 is the \
         diffuse the client renders. Offenders: {sheets_wearing_the_atlas:?}"
    );
}

/// **NaN vertices must never reach the GPU.**
///
/// Every realm scene authors one part — `a_lampglow_01.dds` on a 20-vertex `Box`, 12 triangles —
/// whose vertex positions decode as non-finite. NaN geometry is undefined behaviour: the primitive
/// may be dropped, or it may read whatever follows in the buffer. The loader now refuses those
/// parts and names them, so this asserts the contract on both sides: nothing non-finite survives
/// into a built scene, and exactly the known part was refused.
///
/// **The cause is still open.** The NIF files contain no NaN run long enough to be a vertex array
/// (longest is 4 floats at any alignment; 20 vertices would be 60), so we produce these somewhere
/// in our own decode. Dropping them stops undefined behaviour; it does not explain it.
///
/// This took three wrong diagnoses to pin down, and NaN caused all of them: a NaN coordinate never
/// updates min or max, because `f32::min` returns the other operand. The part first read as a
/// zero-sized box at the world origin, then as an index range referencing no vertex at all.
#[test]
fn no_scene_ships_non_finite_geometry_to_the_gpu() {
    use caer_render::scene_census;

    let root = root();
    let mut refused: Vec<String> = Vec::new();
    for realm in [1u8, 2, 3] {
        let Some(scene) = caer_render::preworld_scene::load(&root, realm) else {
            panic!("realm {realm} scene must load — REQ-025, set CAER_CLIENT");
        };
        for d in &scene.dropped_parts {
            println!("realm {realm}: refused {d}");
            refused.push(format!("realm {realm} {d}"));
        }
        let c = scene_census::census(&scene);
        let bad: Vec<String> = c
            .parts
            .iter()
            .filter(|p| p.out_of_range.non_finite > 0)
            .map(|p| {
                format!(
                    "part {} ({}) {} non-finite",
                    p.part,
                    p.texture.as_deref().unwrap_or("<untextured>"),
                    p.out_of_range.non_finite
                )
            })
            .collect();
        assert!(
            bad.is_empty(),
            "realm {realm} still uploads non-finite geometry: {bad:?}"
        );
        assert_eq!(
            c.out_of_range_indices(),
            0,
            "realm {realm} references vertices past the end of its buffer"
        );
        assert_eq!(
            c.empty_ranges(),
            0,
            "realm {realm} has a draw range that references no vertex"
        );
    }

    let expected: Vec<String> = [1, 2, 3]
        .iter()
        .map(|r| format!("realm {r} Box (a_lampglow_01.dds)"))
        .collect();
    assert_eq!(
        refused, expected,
        "the set of parts refused for non-finite geometry changed. More means a new decode defect; \
         fewer means one was fixed — either way this test needs updating deliberately, with the \
         cause named."
    );
}

/// **Does each scene part's declared blend mode match what its sheet actually holds?**
///
/// Ledger A16. The blend routing has bitten before — the Albion tower drew with an inert blend, and
/// `ModelPartRange::alpha` records that dropping the mode entirely turned Hibernia's sun corona
/// into a solid orange disc while collapsing additive into source-over drew Albion's torch corona
/// as a black rectangle on the ground. Those were found by looking at renders. This asks the data.
///
/// Two disagreements are worth naming. A part declared **opaque over art that carries
/// transparency** ignores compositing the artist authored: hard edges, or whatever RGB sits under
/// the transparent texels. A part declared **blended over a fully opaque sheet** has nothing to
/// composite and pays sort order and overdraw for a solid surface.
///
/// Additive is excluded on purpose: an additive pass uses RGB as intensity and needs no alpha
/// channel, so "opaque sheet, additive mode" is normal and not a finding.
///
/// Diagnostic. It reports; it does not assert a threshold on scene art we have no oracle for.
#[test]
fn scene_blend_modes_versus_their_sheets() {
    use caer_render::scene_census::{self, AlphaAgreement};

    let root = root();
    for (realm, name) in [(1u8, "Albion"), (2, "Midgard"), (3, "Hibernia")] {
        let Some(scene) = caer_render::preworld_scene::load(&root, realm) else {
            panic!("realm {realm} scene must load — REQ-025, set CAER_CLIENT");
        };
        let alpha = scene_census::texture_alpha(&scene);
        let c = scene_census::census(&scene);
        println!(
            "\n=== {name} — {} parts, {} sheets decoded ===",
            c.part_count(),
            alpha.len()
        );
        println!(
            "{:<34} {:<10} {:>7} {:>8} {:>8}  {}",
            "texture", "declared", "mean_a", "opaque%", "tris", "verdict"
        );
        let mut findings = 0usize;
        let mut unknown = 0usize;
        for p in &c.parts {
            let key = p.texture.clone().unwrap_or_default();
            let st = alpha.get(&key);
            let verdict = scene_census::alpha_agreement(p.alpha, st);
            match verdict {
                AlphaAgreement::Consistent => continue,
                AlphaAgreement::Unknown => {
                    unknown += 1;
                    continue;
                }
                _ => findings += 1,
            }
            let (mean, op) = st.map_or((f32::NAN, f32::NAN), |s| (s.mean, s.fully_opaque * 100.0));
            println!(
                "{key:<34} {:<10} {mean:>7.3} {op:>7.1}% {:>8}  {}",
                format!("{:?}", p.alpha),
                p.triangles,
                match verdict {
                    AlphaAgreement::OpaqueOverTransparentArt => "OPAQUE over art with transparency",
                    AlphaAgreement::BlendedOverOpaqueArt => "BLEND over a fully opaque sheet",
                    _ => "",
                }
            );
        }
        println!("  {findings} disagreement(s), {unknown} part(s) whose sheet did not resolve");

        // **Attribution.** The loader forces billboards to Blend regardless of what the NIF
        // declares, so a "blend over an opaque sheet" row may be our own transformation rather
        // than the client's data. Read the model again, unmodified, and say which it is.
        let rel = match realm {
            1 => "pregame/charScreenAlb.npk",
            2 => "pregame/charScreenMid.npk",
            _ => "pregame/charScreenHib.npk",
        };
        if let Ok(Some(m)) =
            caer_assets::open_first(root.join(rel), |n| n.to_ascii_lowercase().ends_with(".nif"))
        {
            if let Ok(raw) = caer_assets::nif::read_model(&m.data) {
                let mut ours_only = 0usize;
                let mut authored = 0usize;
                for p in &raw.parts {
                    let Some(tex) = p.texture.as_ref().map(|t| t.to_ascii_lowercase()) else {
                        continue;
                    };
                    let st = alpha.get(&tex);
                    let is_blend_over_opaque = st.is_some_and(|s| !s.has_transparency());
                    if !is_blend_over_opaque {
                        continue;
                    }
                    if p.alpha.is_blended() {
                        authored += 1;
                    } else if p.billboard {
                        ours_only += 1;
                    }
                }
                println!(
                    "  of the blend-over-opaque rows: {authored} declared Blend in the NIF, \
                     {ours_only} were opaque in the NIF and forced to Blend here because they are \
                     billboards"
                );
            }
        }
        assert!(
            alpha.len() > 5,
            "{name}: only {} sheets decoded — this comparison has no denominator",
            alpha.len()
        );
    }
}

/// **Are the scene's billboards facing the camera?**
///
/// `NiBillboardNode` shapes are meant to rotate to face the viewer — that is what makes a flat quad
/// read as a tree. The parser records the flag (`MeshPart::billboard`) and the scene loader uses it
/// for exactly one thing: forcing the part to `Blend`. Nothing orients them, and the only
/// camera-facing code in the renderer is the *particle* path, which scene meshes do not go through.
///
/// Hibernia authors 684 billboard shapes, Albion 252, Midgard 1. Drawn in their authored
/// orientation, the ones edge-on to our camera are invisible slivers — which is what ledger A7
/// ("missing 2D tree-cluster billboard") looks like from outside.
///
/// **Measured against the raw model, not the merged batch.** `ModelPartRange` does not carry the
/// billboard flag, so a first version of this measured every triangle in the scene and reported
/// 46-65% "edge-on" — which is mostly ground, whose normals point up while the camera looks
/// roughly level. Comparing billboards against non-billboards is the whole point, and the merged
/// batch cannot express it.
#[test]
fn scene_billboards_versus_the_camera_they_are_drawn_for() {
    use caer_render::preworld_scene;

    let root = root();
    for (realm, name, rel) in [
        (1u8, "Albion", "pregame/charScreenAlb.npk"),
        (2, "Midgard", "pregame/charScreenMid.npk"),
        (3, "Hibernia", "pregame/charScreenHib.npk"),
    ] {
        let Some(scene) = preworld_scene::load(&root, realm) else {
            panic!("realm {realm} scene must load — REQ-025, set CAER_CLIENT");
        };
        let (eye, focus, _) = preworld_scene::framing_around_character(
            &scene,
            preworld_scene::CHARACTER_HEIGHT,
            None,
            16.0 / 9.0,
        );
        let view = (focus - eye).normalize();

        let Ok(Some(m)) =
            caer_assets::open_first(root.join(rel), |n| n.to_ascii_lowercase().ends_with(".nif"))
        else {
            panic!("{name}: {rel} must contain a NIF");
        };
        let model = caer_assets::nif::read_model(&m.data).expect("scene NIF must parse");

        // Angle between a triangle's normal and the view direction, bucketed. Facing quads sit
        // near 0 degrees; edge-on ones near 90 and cover almost no pixels.
        let mut bill = [0usize; 3];
        let mut other = [0usize; 3];
        let (mut nb, mut no) = (0usize, 0usize);
        for part in &model.parts {
            let bucket = if part.billboard {
                &mut bill
            } else {
                &mut other
            };
            let count = if part.billboard { &mut nb } else { &mut no };
            for t in part.indices.chunks_exact(3) {
                let n = |i: usize| part.normals.get(t[i] as usize).copied();
                let (Some(a), Some(b), Some(c)) = (n(0), n(1), n(2)) else {
                    continue;
                };
                let avg = [
                    (a[0] + b[0] + c[0]) / 3.0,
                    (a[1] + b[1] + c[1]) / 3.0,
                    (a[2] + b[2] + c[2]) / 3.0,
                ];
                let len = (avg[0] * avg[0] + avg[1] * avg[1] + avg[2] * avg[2]).sqrt();
                if !len.is_finite() || len < 1e-4 {
                    continue;
                }
                let dot = (avg[0] * view.x + avg[1] * view.y + avg[2] * view.z).abs() / len;
                let deg = dot.clamp(0.0, 1.0).acos().to_degrees();
                *count += 1;
                if deg < 30.0 {
                    bucket[0] += 1;
                } else if deg < 70.0 {
                    bucket[1] += 1;
                } else {
                    bucket[2] += 1;
                }
            }
        }

        let show = |label: &str, b: &[usize; 3], total: usize| {
            if total == 0 {
                println!("  {label:<16} none");
                return;
            }
            let pct = |n: usize| n as f32 / total as f32 * 100.0;
            println!(
                "  {label:<16} {total:>6} tris — facing(<30°) {:>5.1}%  oblique {:>5.1}%  edge-on(>70°) {:>5.1}%",
                pct(b[0]),
                pct(b[1]),
                pct(b[2])
            );
        };
        println!("\n{name}:");
        show("billboards", &bill, nb);
        show("everything else", &other, no);

        assert!(
            nb + no > 100,
            "{name}: only {} triangles measured — the sweep did not run",
            nb + no
        );
    }
    println!(
        "\nScene billboards are never oriented toward the camera: `MeshPart::billboard` is read \
         and used only to force Blend (ledger A7)."
    );
}
