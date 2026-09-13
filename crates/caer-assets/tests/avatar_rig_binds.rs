//! GATE: every playable body part must reproduce its static mesh when rigged to its race skeleton.
//!
//! This is the contract that broke twice. A fig3 part names its skin bones one of two ways — a
//! `NiStringsExtraData` list, or in-file bone NODES — and handling only the first silently fell
//! back to in-file block indices applied to the EXTERNAL skeleton. Eyeball and eyelid vertices
//! ended up driven by `Bip01 Ponytail1` / `Beard1` / `Beard2`: the Half Ogre "swollen skewed head".
//!
//! Bind-pose reconstruction catches it because a correctly resolved bone reproduces the authored
//! mesh exactly, and a wrong one does not.

use std::path::PathBuf;

fn client_root() -> PathBuf {
    let root = caer_assets::client_dep::required_caer_client_root("avatar_rig_binds");
    assert!(
        root.join("figures").is_dir(),
        "CAER_CLIENT has no figures directory: {}",
        root.display()
    );
    root
}

fn archives_under(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            archives_under(&p, out);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("mpk")) {
            out.push(p);
        }
    }
}

/// Largest distance between a rigged part's bind-pose reconstruction and the static mesh.
///
/// Parts are paired by vertex count, not index: `sar_f_head01` has ten 24-vertex `Box` helpers in
/// its static part list and one rigged shape, and index pairing compared the head against a Box.
fn worst_error(stat: &caer_assets::nif::Model, rig: &caer_assets::nif::RiggedModel) -> f32 {
    let mut worst = 0.0f32;
    for (pi, p) in rig.parts.iter().enumerate() {
        let mut same: Vec<&caer_assets::nif::MeshPart> = stat
            .parts
            .iter()
            .filter(|s| s.positions.len() == p.positions.len())
            .collect();
        let Some(sp) = (if same.len() == 1 {
            Some(same.remove(0))
        } else {
            stat.parts.get(pi)
        }) else {
            continue;
        };
        for vi in 0..p.positions.len() {
            let Some(v) = sp.positions.get(vi) else {
                continue;
            };
            let b = rig.bind_position(pi, vi);
            worst = worst.max(
                ((b[0] - v[0]).powi(2) + (b[1] - v[1]).powi(2) + (b[2] - v[2]).powi(2)).sqrt(),
            );
        }
    }
    worst
}

/// Read a race/gender's external skeleton.
fn skeleton_for(archives: &[PathBuf], race: u8, gender: u8) -> Option<caer_assets::nif::Skeleton> {
    let sn = caer_assets::figures::FigureModels::skeleton_member(race, gender)?;
    for a in archives {
        if let Ok(Some(x)) = caer_assets::open_first(a, |n| n.eq_ignore_ascii_case(&sn)) {
            if let Ok(s) = caer_assets::nif::read_skeleton(&x.data) {
                return Some(s);
            }
        }
    }
    None
}

/// Worst bind-pose reconstruction error over every base part of every race and gender.
///
/// Parts are paired by vertex count, not index: `sar_f_head01` has ten 24-vertex `Box` helpers in
/// its static part list and one rigged shape, and index pairing compared the head against a Box.
fn worst_bind_errors() -> Vec<(String, f32)> {
    let root = client_root();
    let Ok(figs) = caer_assets::figures::FigureModels::load(root.join("gamedata.mpk")) else {
        return Vec::new();
    };
    let mut archives = Vec::new();
    archives_under(&root.join("figures"), &mut archives);
    archives.sort();

    let races: [(u8, &str); 18] = [
        (1, "Briton"),
        (2, "Avalonian"),
        (3, "Highlander"),
        (4, "Saracen"),
        (5, "Norseman"),
        (6, "Troll"),
        (7, "Dwarf"),
        (8, "Kobold"),
        (9, "Celt"),
        (10, "Firbolg"),
        (11, "Elf"),
        (12, "Lurikeen"),
        (13, "Inconnu"),
        (14, "Valkyn"),
        (15, "Sylvan"),
        (16, "HalfOgre"),
        (17, "Frostalf"),
        (18, "Shar"),
    ];
    let mut out = Vec::new();
    for (race, label) in races {
        for (g, gl) in [(1u8, "m"), (caer_assets::figures::GENDER_FEMALE, "f")] {
            let Some(sn) = caer_assets::figures::FigureModels::skeleton_member(race, g) else {
                continue;
            };
            let mut sk = None;
            for a in &archives {
                if let Ok(Some(x)) = caer_assets::open_first(a, |n| n.eq_ignore_ascii_case(&sn)) {
                    if let Ok(s) = caer_assets::nif::read_skeleton(&x.data) {
                        sk = Some(s);
                        break;
                    }
                }
            }
            let Some(sk) = sk else { continue };
            for part in figs.base_body(race, g) {
                let Ok(Some(pb)) =
                    caer_assets::open_member(root.join(part.archive_path()), &part.nif_member())
                else {
                    continue;
                };
                let (Ok(stat), Ok(Some(rig))) = (
                    caer_assets::nif::read_model(&pb),
                    caer_assets::nif::read_rigged_external(&pb, &sk),
                ) else {
                    continue;
                };
                out.push((
                    format!("{label} {gl} {}", part.nif_member()),
                    worst_error(&stat, &rig),
                ));
            }
        }
    }
    out
}

#[test]
fn every_base_part_rebuilds_its_static_mesh_at_bind_pose() {
    let rows = worst_bind_errors();
    assert!(
        rows.len() > 200,
        "expected the full race x gender x part matrix, got {} rows — the client tree is \
         incomplete and this gate cannot run (REQ-025)",
        rows.len()
    );
    // 2.0 units. Not chosen to make this pass: `MAX_AVATAR_BIND_ERROR` documents a correct
    // external bind as landing within ~2 units (measured 0.00-1.79 across the Briton parts), and
    // 16 of 252 parts sit at 1.15-1.87 as authored slop. Wrong-bone resolution produced 8.98-9.32,
    // so this discriminates the defect without relabelling the baseline as broken. Production
    // itself tolerates 5.0; this is deliberately tighter.
    let bad: Vec<&(String, f32)> = rows.iter().filter(|(_, e)| *e > 2.0).collect();
    assert!(
        bad.is_empty(),
        "{} of {} base parts do not rebuild their static mesh at bind pose — skin bone names are \
         resolving to the wrong skeleton bones:\n{}",
        bad.len(),
        rows.len(),
        bad.iter()
            .map(|(n, e)| format!("  {n} -> {e:.2}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// **The control this gate never had.** It has always asserted that correct bones rebuild the mesh;
/// it never showed that wrong ones do not, so a threshold of 2.0 could have been passing because
/// the measurement was flat rather than because the bind was right.
///
/// Rebuilds one race's parts against another race's skeleton. Bone *names* still resolve — every
/// fig3 skeleton uses the same `Bip01` naming — so the reconstruction runs and lands in the wrong
/// place, which is exactly the failure mode the gate exists for: the Half Ogre head driven by
/// `Bip01 Ponytail1`.
#[test]
fn the_bind_gate_fires_when_a_part_is_rigged_to_another_races_skeleton() {
    let root = client_root();
    assert!(
        root.join("figures").is_dir(),
        "no client tree at {} — this control cannot run (REQ-025)",
        root.display()
    );
    let Ok(figs) = caer_assets::figures::FigureModels::load(root.join("gamedata.mpk")) else {
        panic!("gamedata.mpk must load — the control needs the figure tables");
    };
    let mut archives = Vec::new();
    archives_under(&root.join("figures"), &mut archives);
    archives.sort();

    // Briton male against its own skeleton, and against skeletons of very different builds.
    let (race, gender) = (1u8, 1u8);
    let own = skeleton_for(&archives, race, gender).expect("Briton male skeleton");
    let mut checked = 0usize;
    let mut fired = 0usize;
    let mut best_wrong = 0.0f32;
    let mut worst_right = 0.0f32;

    for part in figs.base_body(race, gender) {
        let Ok(Some(pb)) =
            caer_assets::open_member(root.join(part.archive_path()), &part.nif_member())
        else {
            continue;
        };
        let Ok(stat) = caer_assets::nif::read_model(&pb) else {
            continue;
        };
        let Ok(Some(right)) = caer_assets::nif::read_rigged_external(&pb, &own) else {
            continue;
        };
        let e_right = worst_error(&stat, &right);
        worst_right = worst_right.max(e_right);

        for other in [6u8, 8, 16] {
            // Troll, Kobold, Half Ogre — the three builds furthest from a Briton.
            let Some(sk) = skeleton_for(&archives, other, gender) else {
                continue;
            };
            let Ok(Some(wrong)) = caer_assets::nif::read_rigged_external(&pb, &sk) else {
                continue;
            };
            let e_wrong = worst_error(&stat, &wrong);
            checked += 1;
            best_wrong = best_wrong.max(e_wrong);
            if e_wrong > 2.0 {
                fired += 1;
            }
        }
    }

    assert!(
        checked > 0,
        "no cross-race bind was measured — the control did not run"
    );
    println!(
        "bind gate control: {fired} of {checked} cross-race binds exceed the 2.0 threshold \
         (worst wrong {best_wrong:.2}, worst correct {worst_right:.2})"
    );
    assert!(
        worst_right <= 2.0,
        "the correct skeleton must stay under the threshold, got {worst_right:.2}"
    );
    assert!(
        fired > 0,
        "not one cross-race bind exceeded 2.0 (worst {best_wrong:.2}) — this gate cannot \
         distinguish a right skeleton from a wrong one, so its green result means nothing"
    );
    assert!(
        best_wrong > worst_right,
        "a wrong skeleton ({best_wrong:.2}) must reconstruct worse than the right one \
         ({worst_right:.2})"
    );
}
