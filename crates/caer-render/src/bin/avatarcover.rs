//! `avatarcover` — assemble every playable race+gender avatar headlessly and report, per combo,
//! whether it resolves an anim set, an idle clip, its own skeleton, and whether the rig passes the
//! bind check that gates posing. The A.3.6 coverage number.

use caer_assets::figures::FigureModels;
use caer_assets::nif::{mat_to_quat, quat_axis_angle};

fn main() {
    let root = caer_render::terrain::client_root();
    let figs = FigureModels::load(root.join("gamedata.mpk")).expect("fig3 tables");
    let tables =
        caer_assets::anims::AnimTables::load(root.join("gamedata.mpk")).expect("anim tables");
    let mut archives: Vec<_> = std::fs::read_dir(root.join("figures/fig3"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().starts_with("sfig"))
        })
        .collect();
    archives.sort();

    let (mut ok, mut total) = (0, 0);
    let (mut dir_back, mut dir_slides) = (0, 0);
    for race in 1..=18u8 {
        for gender in [1u8, 2] {
            total += 1;
            let name = FigureModels::race_name(race).unwrap_or("?");
            let gw = if gender == 2 { "Female" } else { "Male" };
            let set = tables.set_for_race_gender(name, gw);
            // Resolve AND LOAD the clip: the table naming one is not proof it parses. The
            // Catacombs `cat_*` clips resolve fine but read_clip rejects them, so a coverage number
            // that only checks the table overstates reality. Mirrors resolve_avatar_clip's fallback.
            let load = |st: u16| -> Option<(String, bool, Option<caer_assets::nif::Clip>)> {
                let c = tables.clip(st, caer_assets::anims::Action::Idle)?;
                let path = std::fs::read_dir(root.join("anims"))
                    .ok()?
                    .flatten()
                    .map(|e| e.path())
                    .find(|p| {
                        p.file_stem()
                            .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case(&c.stem))
                    })?;
                let parsed = std::fs::read(&path)
                    .ok()
                    .and_then(|b| caer_assets::nif::read_clip(&b).ok());
                Some((c.stem.clone(), parsed.is_some(), parsed))
            };
            // Directional locomotion: the player is the case that matters for strafe/backpedal,
            // so check the avatar's own set names AND parses each one — not just the idle.
            let load_action = |st: u16, a: caer_assets::anims::Action| -> bool {
                let Some(c) = tables.clip(st, a) else {
                    return false;
                };
                let Some(path) = std::fs::read_dir(root.join("anims")).ok().and_then(|rd| {
                    rd.flatten().map(|e| e.path()).find(|p| {
                        p.file_stem()
                            .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case(&c.stem))
                    })
                }) else {
                    return false;
                };
                std::fs::read(&path)
                    .ok()
                    .is_some_and(|b| caer_assets::nif::read_clip(&b).is_ok())
            };
            let dirs = set.map_or((false, false, false), |st| {
                (
                    load_action(st, caer_assets::anims::Action::Back),
                    load_action(st, caer_assets::anims::Action::SlideLeft),
                    load_action(st, caer_assets::anims::Action::SlideRight),
                )
            });
            if dirs.0 {
                dir_back += 1;
            }
            if dirs.1 && dirs.2 {
                dir_slides += 1;
            }

            let own = set.and_then(load);
            let briton = tables.set_for_race_gender("Briton", gw).and_then(load);
            let clip = match &own {
                Some((_, true, _)) => own.clone(),
                _ => match &briton {
                    Some((_, true, _)) => briton.clone(),
                    _ => None,
                },
            };
            let borrowed = clip.is_some() && !own.as_ref().is_some_and(|(_, ok, _)| *ok);
            // The race's own skeleton.
            let want = FigureModels::skeleton_member(race, gender).unwrap_or_default();
            let skel = archives.iter().find_map(|a| {
                let ms = caer_assets::open(a).ok()?;
                let m = ms.iter().find(|m| m.name.eq_ignore_ascii_case(&want))?;
                caer_assets::nif::read_skeleton(&m.data).ok()
            });
            // Bind check across the base body.
            let mut worst: f32 = 0.0;
            let mut rigged = 0;
            let mut per_part: Vec<(String, f32)> = Vec::new();
            if let Some(sk) = &skel {
                for part in figs.base_body(race, gender) {
                    let Ok(ms) = caer_assets::open(root.join(part.archive_path())) else {
                        continue;
                    };
                    let w = part.nif_member();
                    let Some(m) = ms.iter().find(|m| m.name.eq_ignore_ascii_case(&w)) else {
                        continue;
                    };
                    let (Ok(Some(rig)), Ok(stat)) = (
                        caer_assets::nif::read_rigged_external(&m.data, sk),
                        caer_assets::nif::read_model(&m.data),
                    ) else {
                        continue;
                    };
                    rigged += 1;
                    let mut pw: f32 = 0.0;
                    for (pi, p) in rig.parts.iter().enumerate() {
                        let Some(sp) = stat.parts.get(pi) else {
                            continue;
                        };
                        for vi in 0..p.positions.len() {
                            let Some(s) = sp.positions.get(vi) else {
                                continue;
                            };
                            let b = rig.bind_position(pi, vi);
                            pw = pw.max(
                                ((b[0] - s[0]).powi(2)
                                    + (b[1] - s[1]).powi(2)
                                    + (b[2] - s[2]).powi(2))
                                .sqrt(),
                            );
                        }
                    }
                    per_part.push((part.filename.clone(), pw));
                    worst = worst.max(pw);
                }
            }
            // CAER_HEAD_FRAME=1: the E13 (c) discriminator, with provenance.
            //
            // Mesh-versus-bone frame offset. A skin whose inverse bind disagrees with the
            // skeleton's head bind applies rotations in a frame the bone does not share.
            //
            // Uniform at (-0.00, 0.00, 90.00) across the roster, so it cannot separate races and
            // cannot explain a per-race symptom. This binary is blind to the product renderer and
            // camera; it reports inputs, not causes.
            //
            // The archive member and hash are printed because 387 NIFs in this client ship under a
            // name that exists in more than one archive and first-hit-wins (E10). Without them this
            // comparison cannot tell "these meshes differ" from "we read the wrong file".
            if std::env::var_os("CAER_HEAD_FRAME").is_some() {
                if let Some(sk) = &skel {
                    let head_bone = sk
                        .bones
                        .iter()
                        .position(|b| b.name.eq_ignore_ascii_case("Bip01 Head"));
                    for part in figs.base_body(race, gender) {
                        if !part.filename.to_ascii_lowercase().contains("head") {
                            continue;
                        }
                        let arc = root.join(part.archive_path());
                        let Ok(ms) = caer_assets::open(&arc) else {
                            continue;
                        };
                        let w = part.nif_member();
                        let Some(m) = ms.iter().find(|m| m.name.eq_ignore_ascii_case(&w)) else {
                            continue;
                        };
                        let sum: u64 = m.data.iter().fold(1469598103934665603u64, |h, b| {
                            (h ^ u64::from(*b)).wrapping_mul(1099511628211)
                        });
                        let Ok(Some(rig)) = caer_assets::nif::read_rigged_external(&m.data, sk)
                        else {
                            continue;
                        };
                        let elev = |m: &[[f32; 4]; 4], a: usize| {
                            let v = [m[a][0], m[a][1], m[a][2]];
                            let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                            if l < 1e-6 {
                                return 0.0;
                            }
                            (v[2] / l).clamp(-1.0, 1.0).asin().to_degrees()
                        };
                        // `world_bind * inverse_bind` for the head bone: the skin-space basis as the bone
                        // sees it at bind. The printed figures are the ELEVATION of that basis's columns,
                        // not a calibrated rotation residual. A uniform (-0, 0, 90) means every race shares
                        // one basis convention, which discriminates only in that it cannot separate races.
                        for rp in &rig.parts {
                            let Some(hb) = head_bone else { continue };
                            let Some(ib) = rp.inverse_bind.get(hb) else {
                                continue;
                            };
                            // `xform_to_mat4` is COLUMN-major; `mat4_mul_col` is the owner and is
                            // tested against the palette fold the GPU runs. Do not hand-roll it —
                            // a row-major multiply here is silently transposed.
                            let composed = caer_render::anim_skin::mat4_mul_col(
                                caer_assets::nif::xform_to_mat4(&sk.bones[hb].world_bind),
                                caer_assets::nif::xform_to_mat4(ib),
                            );
                            println!(
                                "    headframe {:<12} {:<7} {:<22} arc={:<14} hash={:016x} basis_elev=({:>7.2},{:>7.2},{:>7.2})",
                                name,
                                gw,
                                rp.name,
                                arc.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                                sum,
                                elev(&composed, 0),
                                elev(&composed, 1),
                                elev(&composed, 2),
                            );
                        }
                    }
                }
            }
            let poses = clip.is_some() && skel.is_some() && rigged > 0;
            if poses {
                ok += 1;
            }
            if !poses && skel.is_some() {
                let mut v = per_part.clone();
                v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
                println!(
                    "    offending parts: {:?}",
                    v.iter()
                        .take(3)
                        .map(|(n, e)| format!("{n} {e:.1}"))
                        .collect::<Vec<_>>()
                );
            }
            // E13: where the neck ends up once the clip is applied, in the same measure the pose
            // report uses — elevation of the bone's local X above the horizon. A gender split here
            // is the "looking up" defect, and reading it per race is what separates a shared clip
            // from per-race authoring.
            // What a player reads as "looking up" is where the HEAD ends up, not where the neck
            // bone's axis points. Each race binds its own skeleton and the head composes on top of
            // the neck, so a neck figure compared across races is comparing different rigs — that
            // reading called Saracen and Half Ogre males defective when Matt's captures show both
            // correct. Report all three head axes, posed minus bind, so the axis that actually
            // tracks facing can be identified against known-good races rather than assumed.
            let neck = clip
                .as_ref()
                .and_then(|(_, _, c)| c.as_ref())
                .zip(skel.as_ref())
                .and_then(|(c, sk)| {
                    let head = sk
                        .bones
                        .iter()
                        .position(|b| b.name.eq_ignore_ascii_case("Bip01 Head"))?;
                    let neck = sk
                        .bones
                        .iter()
                        .position(|b| b.name.eq_ignore_ascii_case("Bip01 Neck"))?;
                    let posed = sk.pose(c, 0.0);
                    let elev = |m: &[[f32; 4]; 4], axis: usize| {
                        let v = [m[axis][0], m[axis][1], m[axis][2]];
                        let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                        if len < 1e-6 {
                            return 0.0;
                        }
                        (v[2] / len).clamp(-1.0, 1.0).asin().to_degrees()
                    };
                    let hp = caer_assets::nif::xform_to_mat4(posed.get(head)?);
                    let hb = caer_assets::nif::xform_to_mat4(&sk.bones[head].world_bind);
                    let np = caer_assets::nif::xform_to_mat4(posed.get(neck)?);
                    let nb = caer_assets::nif::xform_to_mat4(&sk.bones[neck].world_bind);
                    // How many keys the neck track carries. A single-key track is a static pose
                    // being applied as if it were animation, which is a different defect than a
                    // clip that genuinely animates the neck to a different place.
                    let neck_keys = sk.bones[neck]
                        .id
                        .and_then(|id| c.tracks.get(&id))
                        .map_or(0, |t| match &t.rotation {
                            caer_assets::nif::RotChannel::None => 0,
                            caer_assets::nif::RotChannel::Quat(k) => k.len(),
                            caer_assets::nif::RotChannel::Euler { x, y, z } => {
                                x.len().max(y.len()).max(z.len())
                            }
                        });
                    // CAER_RAW_KEYS=1: the AUTHORED local quaternion for neck and head, straight
                    // out of the .kfa, beside the bind local.
                    //
                    // INPUT PROVENANCE ONLY. These are authored bytes, not a final pose and not a
                    // render. `sample_local` keeps the target skeleton's bind translation for every
                    // bone outside the root-motion carriers (the root and its direct children), and
                    // parent-chain composition separates a rotation from its authored key. Neither
                    // an agreement nor a difference here attributes a defect to retail or to us.
                    if std::env::var_os("CAER_RAW_KEYS").is_some() {
                        let clip_name = clip.as_ref().map_or("?", |(n, _, _)| n.as_str());
                        // The whole spine chain, not just neck and head. The neck's authored key
                        // is near-identity in every clip, yet its composed world orientation
                        // differs by ~5 degrees between the clean and defective sets — so most of
                        // that angle enters upstream. Walking the chain names the bone where it
                        // actually enters instead of assuming it is the one nearest the symptom.
                        if std::env::var_os("CAER_BONE_NAMES").is_some() {
                            let names: Vec<&str> =
                                sk.bones.iter().map(|b| b.name.as_str()).collect();
                            println!("    bones {names:?}");
                        }
                        let chain: Vec<(String, usize)> = sk
                            .bones
                            .iter()
                            .enumerate()
                            .filter(|(_, b)| {
                                let n = b.name.to_ascii_lowercase();
                                !n.contains("nub")
                                    && (n == "bip01"
                                        || n.starts_with("bip01 spine")
                                        || n == "bip01 neck"
                                        || n == "bip01 head"
                                        || n == "bip01 pelvis"
                                    || n.contains("spine"))
                            })
                            .map(|(i, b)| (b.name.clone(), i))
                            .collect();
                        for (label, bi) in chain.iter().map(|(n, i)| (n.as_str(), *i)) {
                            // Composed world delta for this bone, same measure as the head column.
                            let wp = caer_assets::nif::xform_to_mat4(&posed[bi]);
                            let wb = caer_assets::nif::xform_to_mat4(&sk.bones[bi].world_bind);
                            let d: Vec<f32> = (0..3).map(|a| elev(&wp, a) - elev(&wb, a)).collect();
                            print!(
                                "    chain {clip_name:<12} {label:<12} world d=({:>7.2},{:>7.2},{:>7.2}) ",
                                d[0], d[1], d[2]
                            );
                            // The BIND local rotation beside the authored key, through the tested
                            // owners — `mat_to_quat` carries the near-180-degree branch logic.
                            let (baxis, bangle) =
                                quat_axis_angle(mat_to_quat(&sk.bones[bi].local.0));
                            print!(
                                "bind axis=({:>7.4},{:>7.4},{:>7.4}) angle={bangle:>7.2} ",
                                baxis[0], baxis[1], baxis[2]
                            );
                            match sk.bones[bi].id.and_then(|id| c.tracks.get(&id)).map(|t| &t.rotation) {
                                Some(caer_assets::nif::RotChannel::Quat(keys)) => {
                                    let [w, x, y, z] = keys.first().map_or([1.0, 0.0, 0.0, 0.0], |k| k.value);
                                    println!(
                                        "authored wxyz=({w:>7.4},{x:>7.4},{y:>7.4},{z:>7.4}) keys={}",
                                        keys.len()
                                    );
                                }
                                _ => println!("authored <none — holds bind>"),
                            }
                        }
                        for (label, bi) in [("neck", neck), ("head", head)] {
                            let Some(t) = sk.bones[bi].id.and_then(|id| c.tracks.get(&id)) else {
                                continue;
                            };
                            if let caer_assets::nif::RotChannel::Quat(keys) = &t.rotation {
                                if let Some(k) = keys.first() {
                                    // Through the shared helper, where the component order is
                                    // pinned by test. `k.value` is [w, x, y, z].
                                    let [w, x, y, z] = k.value;
                                    let (axis, angle) =
                                        caer_assets::nif::quat_axis_angle(k.value);
                                    println!(
                                        "    rawkey {clip_name:<12} {label:<4} keys{:>3} t={:.3} wxyz=({w:>7.4},{x:>7.4},{y:>7.4},{z:>7.4}) axis=({:>7.4},{:>7.4},{:>7.4}) angle={:>7.2}deg",
                                        keys.len(),
                                        k.time,
                                        axis[0], axis[1], axis[2],
                                        angle
                                    );
                                }
                            }
                        }
                    }
                    // Which rotation encoding each track uses. If the clips carrying the defect
                    // share one encoding and the clean ones share the other, the fault is in a
                    // decode path rather than in what an animator authored — which is what four
                    // files with distinct md5s producing an identical head result already suggests.
                    let enc = |bi: usize| -> &'static str {
                        match sk.bones[bi]
                            .id
                            .and_then(|id| c.tracks.get(&id))
                            .map(|t| &t.rotation)
                        {
                            Some(caer_assets::nif::RotChannel::Quat(_)) => "quat",
                            Some(caer_assets::nif::RotChannel::Euler { .. }) => "EULER",
                            Some(caer_assets::nif::RotChannel::None) => "none",
                            None => "-",
                        }
                    };
                    Some((
                        [0, 1, 2].map(|a| elev(&hp, a) - elev(&hb, a)),
                        elev(&np, 0) - elev(&nb, 0),
                        neck_keys,
                        format!("neck:{} head:{}", enc(neck), enc(head)),
                    ))
                });
            // A bone the clip does not key inherits its parent rigidly and stays at bind. For the
            // finger chain that reads as a flat splayed hand, which is not a pose anyone authored.
            let fingers = clip
                .as_ref()
                .and_then(|(_, _, c)| c.as_ref())
                .zip(skel.as_ref())
                .map(|(c, sk)| {
                    let f: Vec<_> = sk
                        .bones
                        .iter()
                        .filter(|b| b.name.to_ascii_lowercase().contains("finger"))
                        .collect();
                    let keyed = f
                        .iter()
                        .filter(|b| b.id.is_some_and(|id| c.tracks.contains_key(&id)))
                        .count();
                    let matched = sk
                        .bones
                        .iter()
                        .filter(|b| b.id.is_some_and(|id| c.tracks.contains_key(&id)))
                        .count();
                    // Which joints the clip leaves at bind, not just how many. A knuckle that bends
                    // while the joints past it stay straight is a different defect than a whole
                    // hand that never moves, and only the names separate them.
                    if std::env::var_os("CAER_FINGER_NAMES").is_some() {
                        let unkeyed: Vec<&str> = sk
                            .bones
                            .iter()
                            .filter(|b| {
                                b.name.to_ascii_lowercase().contains("finger")
                                    && b.id.is_some()
                                    && !b.id.is_some_and(|id| c.tracks.contains_key(&id))
                            })
                            .map(|b| b.name.as_str())
                            .collect();
                        if !unkeyed.is_empty() {
                            println!("    unkeyed id'd finger joints: {unkeyed:?}");
                        }
                    }
                    let ided = sk.bones.iter().filter(|b| b.id.is_some()).count();
                    let fingers_ided = sk
                        .bones
                        .iter()
                        .filter(|b| {
                            b.name.to_ascii_lowercase().contains("finger") && b.id.is_some()
                        })
                        .count();
                    (keyed, f.len(), matched, c.tracks.len(), ided, fingers_ided)
                });
            println!("{:<12} {:<7} set {:>5}  clip {:<12} skel {:>4}  rigged {rigged}  bind {worst:>6.2}  neck {}  {}  {}",
                name, gw,
                set.map_or("-".into(), |s| s.to_string()),
                clip.as_ref().map_or("-".to_string(), |(s, _, _)| if borrowed { format!("{s}*") } else { s.clone() }),
                skel.as_ref().map_or(0, |s| s.bones.len()),
                neck.map_or("     -".to_string(), |(h, n, k, e)| format!(
                    "head dY-dX{:>7.2}  neck dX{n:>7.2}  keys {k:>3}  rot {e}",
                    h[1] - h[0]
                )),
                fingers.map_or("   -".to_string(), |(k, n, m, t, ided, fi)| format!("fingers {k:>2} keyed of {fi:>2} id'd of {n:<2}  tracks {t:>3} match {m:>3} of {ided:>3} id'd bones")),
                if poses { "POSES" } else { "static" });
        }
    }
    println!("\n=== {ok}/{total} race+gender avatars pose ===");
    println!("=== directional locomotion: {dir_back}/{total} have a parsing BACK clip, {dir_slides}/{total} have BOTH sidesteps ===");
}
