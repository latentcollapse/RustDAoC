//! Smoke-test floor for `caer-render` (Foundation Audit step 3).
//!
//! The audit's second RED finding was that this crate carried **1 test across 6,663 LOC** while
//! holding `gpu.rs`, `terrain.rs` and `entities.rs` — and that it is the crate the app-shell
//! unification (step 4) would restructure. Per the playbook gate rule, that refactor must not
//! proceed over untested code, because refactoring untested code relocates risk instead of
//! reducing it. **This file is that gate.**
//!
//! Deliberately **structural, not golden**: these assert invariants and intent (index bounds are
//! valid, a posed mesh actually differs from its bind pose, terrain is finite and sane) rather
//! than pixels. Structural assertions survive GPU/driver differences and intentional visual
//! changes; the two golden-image tests that do exist live in `terrain_golden.rs` and cover the
//! one case where "looks right" genuinely is the property.
//!
//! Every test is **client-gated**: skipped when the game isn't installed, so the suite stays green
//! on a machine without proprietary assets. That's the same pattern the asset-crate tests use.

use caer_render::terrain;

/// The client install root, or `None` when the game isn't present (→ skip the test).
fn client() -> Option<std::path::PathBuf> {
    let root = terrain::client_root();
    root.join("figures").is_dir().then_some(root)
}

/// Read a `figures/` NIF by base name, case-insensitively.
fn figure(root: &std::path::Path, stem: &str) -> Option<Vec<u8>> {
    let p = std::fs::read_dir(root.join("figures"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_stem()
                .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(stem))
                && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("nif"))
        })?;
    std::fs::read(p).ok()
}

/// Assert a batch is internally coherent: every index addresses a real vertex, every part range
/// lies inside `indices`, and the bounds actually contain the geometry. A mesh that violates any
/// of these renders as garbage or crashes the GPU — cheap to check, catastrophic to miss.
fn assert_batch_coherent(b: &terrain::ModelBatch, what: &str) {
    assert!(!b.vertices.is_empty(), "{what}: no vertices");
    assert!(!b.indices.is_empty(), "{what}: no indices");
    assert_eq!(
        b.indices.len() % 3,
        0,
        "{what}: index count is not a multiple of 3"
    );
    let nv = b.vertices.len() as u32;
    assert!(
        b.indices.iter().all(|&i| i < nv),
        "{what}: index out of vertex range"
    );
    for (n, r) in b.parts.iter().enumerate() {
        assert!(r.start <= r.end, "{what}: part {n} range inverted");
        assert!(
            r.end <= b.indices.len() as u32,
            "{what}: part {n} range past end of indices"
        );
    }
    for (a, axis) in ["x", "y", "z"].iter().enumerate() {
        assert!(
            b.bound_min[a].is_finite() && b.bound_max[a].is_finite(),
            "{what}: non-finite bound on {axis}"
        );
        assert!(
            b.bound_min[a] <= b.bound_max[a],
            "{what}: inverted bound on {axis}"
        );
        for v in &b.vertices {
            assert!(v.pos[a].is_finite(), "{what}: non-finite vertex on {axis}");
            assert!(
                v.pos[a] >= b.bound_min[a] - 0.01 && v.pos[a] <= b.bound_max[a] + 0.01,
                "{what}: vertex outside stated bounds on {axis}"
            );
        }
    }
    assert!(
        b.bound_radius.is_finite() && b.bound_radius > 0.0,
        "{what}: bad bound radius"
    );
}

#[test]
fn static_model_batch_is_coherent() {
    let Some(root) = client() else { return };
    let Some(bytes) = figure(&root, "skel01") else {
        return;
    };
    let model = caer_assets::nif::read_model(&bytes).expect("skel01 parses");
    let batch = terrain::untextured_batch(&model);
    assert_batch_coherent(&batch, "skel01 static");
}

#[test]
fn posed_batch_is_coherent_and_actually_differs_from_bind() {
    // The core animation invariant, expressed structurally: posing must (a) preserve topology and
    // (b) actually move vertices. A regression that silently stopped applying the pose would keep
    // every other assertion green — this is the one that catches it.
    let Some(root) = client() else { return };
    let Some(bytes) = figure(&root, "skel01") else {
        return;
    };
    let rig = match caer_assets::nif::read_rigged(&bytes) {
        Ok(Some(r)) => r,
        _ => return, // not skinned in this install — nothing to assert
    };
    // Resolve the same clip the renderer would (skeletons idle on the shared humanoid clip).
    let clip_path = std::fs::read_dir(root.join("anims"))
        .expect("anims dir")
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_stem()
                .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case("i_hm"))
        });
    let Some(clip_path) = clip_path else { return };
    let clip =
        caer_assets::nif::read_clip(&std::fs::read(clip_path).unwrap()).expect("clip parses");

    let posed = terrain::posed_batch(&rig, &clip, 0.5);
    assert_batch_coherent(&posed, "skel01 posed");

    let model = caer_assets::nif::read_model(&bytes).expect("static parses");
    let bind = terrain::untextured_batch(&model);

    // (a) topology preserved — same triangle count as the bind mesh.
    assert_eq!(
        posed.indices.len(),
        bind.indices.len(),
        "posing changed the triangle count"
    );

    // (b) the pose was actually applied: a meaningful share of vertices moved off bind.
    let moved = posed
        .vertices
        .iter()
        .zip(bind.vertices.iter())
        .filter(|(p, b)| {
            let d = (p.pos[0] - b.pos[0]).powi(2)
                + (p.pos[1] - b.pos[1]).powi(2)
                + (p.pos[2] - b.pos[2]).powi(2);
            d.sqrt() > 1.0
        })
        .count();
    let frac = moved as f32 / posed.vertices.len().max(1) as f32;
    assert!(
        frac > 0.10,
        "posing barely moved anything ({moved} verts, {:.1}%) — is the clip binding?",
        frac * 100.0
    );

    // ...but it must not explode: an idle pose stays within the bind mesh's own scale. A wrong
    // skeleton blows the bounds up by orders of magnitude, which is exactly how the first avatar
    // attempt failed (bodies collapsed into puddles).
    let bind_h = bind.bound_max[2] - bind.bound_min[2];
    let posed_h = posed.bound_max[2] - posed.bound_min[2];
    assert!(
        posed_h > bind_h * 0.5 && posed_h < bind_h * 1.5,
        "posed height {posed_h:.0} is implausible against bind height {bind_h:.0}"
    );
}

#[test]
fn terrain_region_mesh_is_finite_and_indexed_in_bounds() {
    let Some(_root) = client() else { return };
    // A small window around the Camelot Hills test area — enough zones to be real, small enough
    // to stay fast.
    let origin = glam::Vec3::new(592_000.0, 538_000.0, 0.0);
    let mesh = terrain::load_region(
        1,
        origin,
        [560_000, 500_000],
        [630_000, 570_000],
        terrain::seam_blend(),
    );
    if mesh.zones.is_empty() {
        return; // zone data absent in this install
    }
    assert!(mesh.zones_loaded > 0, "zones present but zones_loaded is 0");
    for (n, z) in mesh.zones.iter().enumerate() {
        assert!(!z.vertices.is_empty(), "zone {n}: no vertices");
        let nv = z.vertices.len() as u32;
        assert!(
            z.indices.iter().all(|&i| i < nv),
            "zone {n}: index out of range"
        );
        assert_eq!(
            z.indices.len() % 3,
            0,
            "zone {n}: index count not a multiple of 3"
        );
        for v in &z.vertices {
            assert!(
                v.pos[0].is_finite() && v.pos[1].is_finite() && v.pos[2].is_finite(),
                "zone {n}: non-finite vertex"
            );
            // DAoC terrain heights live in a sane band; NaN/garbage shows up here first.
            assert!(
                v.pos[2] > -100_000.0 && v.pos[2] < 100_000.0,
                "zone {n}: absurd terrain height {}",
                v.pos[2]
            );
        }
    }
    // Water is optional, but if present it must be well-formed.
    let wv = mesh.water_vertices.len() as u32;
    assert!(
        mesh.water_indices.iter().all(|&i| i < wv),
        "water index out of range"
    );
    // Fixture model batches go through the same coherence rules as entity meshes.
    for (n, m) in mesh.models.iter().enumerate().take(20) {
        assert_batch_coherent(m, &format!("fixture model {n}"));
    }
}

#[test]
fn entity_model_tables_resolve() {
    // No GPU needed: this is the resolution layer the renderer sits on. If the table hops break,
    // every creature silently becomes a box — a failure mode that renders "fine".
    let Some(_root) = client() else { return };
    let Some(models) = caer_render::entities::EntityModels::load() else {
        return;
    };
    // Model 26 (Large Skeleton) is the long-standing reference case.
    assert!(
        models.has_model(26),
        "model 26 (skel01) should resolve to a figures NIF"
    );
}

#[test]
fn avatar_model_ids_are_distinct_and_clear_of_creature_ids() {
    // The synthetic avatar key must never collide with a real creature model id, or an avatar
    // would silently replace a mob's mesh in the shared cache.
    let mut seen = std::collections::HashSet::new();
    for race in 1..=18u8 {
        for gender in [1u8, 2] {
            let id = caer_render::entities::avatar_model_id(race, gender);
            assert!(
                id >= 0xF000,
                "avatar id {id:#x} is inside the creature-id range"
            );
            assert!(
                seen.insert(id),
                "avatar id {id:#x} collides with another race/gender"
            );
        }
    }
}

#[test]
fn gpu_skinned_batch_reproduces_the_cpu_posed_batch() {
    // End-to-end equivalence for the GPU-skinning change: applying the palette to the skinned
    // batch on the CPU must reproduce `posed_batch` vertex-for-vertex. `posed_batch` is the
    // path that produced every verified screenshot so far, so it is the reference implementation.
    //
    // Doing this *before* writing the shader means a later visual difference is provably a
    // pipeline/binding bug, not a maths or data-layout bug — which is the difference between
    // debugging a black screen for ten minutes and for an evening.
    let Some(root) = client() else { return };
    let Some(bytes) = figure(&root, "skel01") else {
        return;
    };
    let rig = match caer_assets::nif::read_rigged(&bytes) {
        Ok(Some(r)) => r,
        _ => return,
    };
    let clip_path = std::fs::read_dir(root.join("anims"))
        .expect("anims dir")
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_stem()
                .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case("i_hm"))
        });
    let Some(clip_path) = clip_path else { return };
    let clip = caer_assets::nif::read_clip(&std::fs::read(clip_path).unwrap()).expect("clip");

    let t = 0.5;
    let skinned = terrain::skinned_batch(&rig);
    let posed = terrain::posed_batch(&rig, &clip, t);
    let palette = rig.bone_matrices(&clip, t);
    let stride = skinned.bone_stride as usize;

    assert_eq!(
        skinned.vertices.len(),
        posed.vertices.len(),
        "vertex count differs from the CPU path"
    );
    assert_eq!(
        skinned.indices.len(),
        posed.indices.len(),
        "index count differs from the CPU path"
    );
    assert_eq!(
        palette.len(),
        rig.parts.len() * stride,
        "palette length does not match parts x stride"
    );

    // Walk parts in the same order the batch was built, so vertex i lines up with posed vertex i.
    let mut vi_global = 0usize;
    for part in &skinned.parts {
        // joints already carry their part's offset, so no base is added here — the test indexes
        // the palette exactly as the shader will.
        let count = rig.parts[part.palette_slot as usize].positions.len();
        for _ in 0..count {
            let v = skinned.vertices[vi_global].pos;
            let s = skinned.skin[vi_global];
            // Exactly the blend the vertex shader will perform.
            let mut out = [0.0f32; 3];
            for k in 0..4 {
                let w = s.weights[k];
                if w == 0.0 {
                    continue;
                }
                let m = &palette[s.joints[k] as usize];
                for r in 0..3 {
                    out[r] += w * (m[0][r] * v[0] + m[1][r] * v[1] + m[2][r] * v[2] + m[3][r]);
                }
            }
            let want = posed.vertices[vi_global].pos;
            for r in 0..3 {
                assert!(
                    (out[r] - want[r]).abs() < 1e-2,
                    "vertex {vi_global} axis {r}: gpu-path {} vs posed_batch {}",
                    out[r],
                    want[r]
                );
            }
            vi_global += 1;
        }
    }
    assert!(
        vi_global > 100,
        "expected a real mesh, compared only {vi_global} vertices"
    );
}

/// Build a bare `SkinnedRig` for locomotion-selection tests: no geometry, just the clip set and
/// strides, which is all `clip_for_speed` consults.
///
/// Each clip gets a distinct duration so a test can identify which one came back by its length.
fn loco_rig(walk: bool, run: bool) -> caer_render::entities::SkinnedRig {
    loco_rig_full(walk, run, false, false)
}

/// The same, with the DIRECTIONAL clips (back / sidestep) selectable — those are what a character
/// backing up or strafing must play instead of a rotated walk.
fn loco_rig_full(
    walk: bool,
    run: bool,
    back: bool,
    slides: bool,
) -> caer_render::entities::SkinnedRig {
    let clip = |d: f32| caer_assets::nif::Clip {
        tracks: Default::default(),
        duration: d,
        rate: 1.0,
    };
    caer_render::entities::SkinnedRig {
        rigs: Vec::new(),
        clip: clip(1.0),
        walk: walk.then(|| clip(2.0)),
        run: run.then(|| clip(3.0)),
        back: back.then(|| clip(4.0)),
        slide_left: slides.then(|| clip(5.0)),
        slide_right: slides.then(|| clip(6.0)),
        stride_walk: 60.0,
        stride_run: 180.0,
        stride_back: 40.0,
        stride_strafe: 50.0,
        z_offset: 0.0,
    }
}

#[test]
fn locomotion_picks_idle_walk_or_run_by_speed() {
    let rig = loco_rig(true, true);
    // Standing still idles — the common case for the mobs on screen.
    assert_eq!(rig.clip_for_speed(0).0.duration, 1.0);
    assert_eq!(rig.clip_for_speed(0).1, 0.0, "idle has no stride");
    // Moving slowly walks; at or above the run threshold, runs.
    assert_eq!(rig.clip_for_speed(40).0.duration, 2.0);
    assert_eq!(rig.clip_for_speed(40).1, 60.0);
    assert_eq!(rig.clip_for_speed(191).0.duration, 3.0);
    assert_eq!(rig.clip_for_speed(191).1, 180.0);
}

/// Backing up and sidestepping are their OWN clips, not a rotated walk: in DAoC the character keeps
/// facing forward and travels the other way. Speed alone cannot tell them apart from a walk — they
/// are the same wire speed — so the direction of travel is what selects them.
#[test]
fn direction_of_travel_picks_the_back_and_sidestep_clips() {
    use caer_render::entities::{Loco, Motion, SkinnedRig};
    let rig = loco_rig_full(true, true, true, true);

    assert_eq!(SkinnedRig::state_for_motion(40, Motion::Back), Loco::Back);
    assert_eq!(
        SkinnedRig::state_for_motion(40, Motion::Left),
        Loco::SlideLeft
    );
    assert_eq!(
        SkinnedRig::state_for_motion(40, Motion::Right),
        Loco::SlideRight
    );

    // …and each resolves to its own clip and its own stride, not the walk cycle.
    assert_eq!(rig.clip_for_state(Loco::Back).0.duration, 4.0);
    assert_eq!(
        rig.clip_for_state(Loco::Back).1,
        40.0,
        "back has its own stride"
    );
    assert_eq!(rig.clip_for_state(Loco::SlideLeft).0.duration, 5.0);
    assert_eq!(rig.clip_for_state(Loco::SlideRight).0.duration, 6.0);
    assert_eq!(rig.clip_for_state(Loco::SlideRight).1, 50.0);

    // Left and right must be DIFFERENT clips — one mirrored for both would step the wrong foot.
    assert_ne!(
        rig.clip_for_state(Loco::SlideLeft).0.duration,
        rig.clip_for_state(Loco::SlideRight).0.duration,
    );
}

/// Standing still is idle no matter which way the keys point, and a forward run stays a run —
/// DAoC has no reverse run, so only speed splits walk from run.
#[test]
fn direction_does_not_override_standing_still_or_the_run_split() {
    use caer_render::entities::{Loco, Motion, SkinnedRig};
    for m in [Motion::Forward, Motion::Back, Motion::Left, Motion::Right] {
        assert_eq!(
            SkinnedRig::state_for_motion(0, m),
            Loco::Idle,
            "{m:?} at zero speed"
        );
    }
    assert_eq!(
        SkinnedRig::state_for_motion(40, Motion::Forward),
        Loco::Walk
    );
    assert_eq!(
        SkinnedRig::state_for_motion(191, Motion::Forward),
        Loco::Run
    );
    // Backing up fast is still the Back cycle: there is no run-backwards clip to escalate to.
    assert_eq!(SkinnedRig::state_for_motion(191, Motion::Back), Loco::Back);
}

/// A creature whose set lacks the directional clips must WALK while moving sideways, not stand
/// there sliding in its idle pose. 425 of 431 sets ship the slides, so this is the thin tail.
#[test]
fn a_missing_directional_clip_falls_back_to_the_walk_not_idle() {
    use caer_render::entities::Loco;
    let rig = loco_rig_full(true, true, false, false);
    assert_eq!(
        rig.clip_for_state(Loco::Back).0.duration,
        2.0,
        "no back clip -> walk"
    );
    assert_eq!(
        rig.clip_for_state(Loco::SlideLeft).0.duration,
        2.0,
        "no slide clip -> walk"
    );
    assert_eq!(
        rig.clip_for_state(Loco::SlideLeft).1,
        60.0,
        "…and the walk's stride with it"
    );
    // With nothing at all it degrades to idle rather than panicking on a missing clip.
    let bare = loco_rig_full(false, false, false, false);
    assert_eq!(bare.clip_for_state(Loco::Back).0.duration, 1.0);
}

#[test]
fn locomotion_falls_back_when_clips_are_missing() {
    // A creature with no walk clip must still animate when moving slowly (run, then idle) rather
    // than freeze — many creature anim sets are sparse.
    let no_walk = loco_rig(false, true);
    assert_eq!(
        no_walk.clip_for_speed(40).0.duration,
        3.0,
        "no walk clip -> use run"
    );
    let no_run = loco_rig(true, false);
    assert_eq!(
        no_run.clip_for_speed(191).0.duration,
        2.0,
        "no run clip -> use walk"
    );
    let neither = loco_rig(false, false);
    assert_eq!(
        neither.clip_for_speed(191).0.duration,
        1.0,
        "no locomotion at all -> idle"
    );
    assert_eq!(
        neither.clip_for_speed(191).1,
        0.0,
        "idle fallback reports no stride"
    );
}

#[test]
fn blend_weight_ramps_then_completes() {
    use caer_render::entities::{EntityAnim, Loco, BLEND_SECONDS};
    let anim = |prev, cur, start| EntityAnim {
        cur,
        prev,
        blend_start: start,
        last_seen: start,
    };

    // No previous state — nothing to blend.
    assert_eq!(anim(None, Loco::Idle, 0.0).blend_weight(1.0), None);
    // Previous == current (e.g. a settled entity) — also nothing to blend.
    assert_eq!(
        anim(Some(Loco::Walk), Loco::Walk, 0.0).blend_weight(0.1),
        None
    );

    // A real change ramps 0 -> 1 over BLEND_SECONDS...
    let a = anim(Some(Loco::Idle), Loco::Walk, 10.0);
    assert_eq!(
        a.blend_weight(10.0),
        Some(0.0),
        "blend starts fully on the previous state"
    );
    let mid = a
        .blend_weight(10.0 + BLEND_SECONDS * 0.5)
        .expect("mid-blend");
    assert!((mid - 0.5).abs() < 1e-4, "expected half-way, got {mid}");
    // ...and then reports None, collapsing back to the cheap single-clip path.
    assert_eq!(
        a.blend_weight(10.0 + BLEND_SECONDS),
        None,
        "blend must complete, not sit at 1.0"
    );
    assert_eq!(
        a.blend_weight(999.0),
        None,
        "a long-finished blend stays finished"
    );
}

#[test]
fn locomotion_state_thresholds() {
    use caer_render::entities::{Loco, SkinnedRig};
    assert_eq!(SkinnedRig::state_for_speed(0), Loco::Idle);
    assert_eq!(SkinnedRig::state_for_speed(1), Loco::Walk);
    assert_eq!(SkinnedRig::state_for_speed(99), Loco::Walk);
    assert_eq!(SkinnedRig::state_for_speed(100), Loco::Run);
    assert_eq!(SkinnedRig::state_for_speed(u16::MAX), Loco::Run);
}
