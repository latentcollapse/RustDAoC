//! `nifstat` — bulk-parse NIFs and report the walker's coverage. This is the verification harness
//! for the clean-room NIF reader: any layout mistake desyncs and shows up here as a failure, per
//! failing block type. Accepts three input shapes:
//!   nifstat <dir-of-.npk>       — the zone NIF libraries (Nifs/, Dnifs/, …)
//!   nifstat <dir-of-loose-.nif> — the `figures/` monster/creature meshes (loose files)
//!   nifstat <one.nif|.npk>      — a single loose NIF or archive
//! Add `--verbose` for per-model bounding boxes / failures.
//! Add `--parts` to dump every MeshPart: name, vertex count, centroid, m_slot.
//! Add `--doors` to dump door NiNode markers + door MeshPart centroids (M15 m_slot source).
//! Add `--morphs` to report the vertex animation the model actually carries — animated parts,
//! their targets, vertex counts and clip length. "Carries a morpher" and "moves" are different
//! questions and this answers the second.
//! Add `--morph-detail` to expose each target's key values and geometric delta range.  This is
//! deliberately an extension of the existing NIF inspection surface rather than another one-off
//! figure binary: static-looking player-head targets are where character-customisation work needs
//! the same evidence as animated scene foliage.
//! Add `--blocks` to census block types per model — which controllers a scene actually carries,
//! which is the question "is there anything to animate here" reduces to.

use std::collections::HashMap;
use std::path::PathBuf;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let verbose = take_flag(&mut args, "--verbose");
    let parts_dump = take_flag(&mut args, "--parts");
    let doors_dump = take_flag(&mut args, "--doors");
    let blocks_dump = take_flag(&mut args, "--blocks");
    let morphs_dump = take_flag(&mut args, "--morphs");
    let morph_detail = take_flag(&mut args, "--morph-detail");
    let stages_dump = take_flag(&mut args, "--stages");
    let uv_dump = take_flag(&mut args, "--uv");
    let emit_dump = take_flag(&mut args, "--emitters");
    let stand_dump = take_flag(&mut args, "--stand");
    let Some(path) = args.first().cloned() else {
        eprintln!(
            "usage: nifstat <nifs-dir | one.nif|.npk> [--verbose] [--parts] [--doors] [--blocks]"
        );
        std::process::exit(2);
    };

    let mut ok = 0usize;
    let mut failed = 0usize;
    let mut parts = 0usize;
    let mut tris = 0usize;
    let mut by_error: HashMap<String, usize> = HashMap::new();

    // Build the work list: each entry is a (label, raw-NIF-bytes) pair. A `.npk` archive yields its
    // first `.nif` member; a loose `.nif` yields its own bytes.
    let meta = std::fs::metadata(&path).expect("stat input path");
    let mut jobs: Vec<(String, Vec<u8>)> = Vec::new();
    if meta.is_file() {
        let p = PathBuf::from(&path);
        let name = p
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_ascii_lowercase();
        let bytes = std::fs::read(&path).expect("read nif/npk file");
        if name.ends_with(".npk") {
            match caer_assets::open(&path) {
                Ok(members) => match members
                    .iter()
                    .find(|m| m.name.to_ascii_lowercase().ends_with(".nif"))
                {
                    Some(nif) => jobs.push((
                        p.file_name().unwrap().to_string_lossy().into_owned(),
                        nif.data.clone(),
                    )),
                    None => {
                        eprintln!("nifstat: no .nif member in {path}");
                        std::process::exit(1);
                    }
                },
                Err(e) => {
                    eprintln!("nifstat: cannot open {path}: {e}");
                    std::process::exit(1);
                }
            }
        } else {
            jobs.push((p.file_name().unwrap().to_string_lossy().into_owned(), bytes));
        }
    } else {
        let mut entries: Vec<_> = std::fs::read_dir(&path)
            .expect("read nifs dir")
            .flatten()
            .collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for e in &entries {
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            if name.ends_with(".npk") {
                match caer_assets::open(e.path()) {
                    Ok(members) => match members
                        .iter()
                        .find(|m| m.name.to_ascii_lowercase().ends_with(".nif"))
                    {
                        Some(nif) => jobs.push((
                            e.file_name().to_string_lossy().into_owned(),
                            nif.data.clone(),
                        )),
                        None => {
                            *by_error.entry("no .nif member".into()).or_default() += 1;
                            failed += 1;
                        }
                    },
                    Err(_) => {
                        *by_error.entry("mpak read failed".into()).or_default() += 1;
                        failed += 1;
                    }
                }
            } else if name.ends_with(".nif") {
                match std::fs::read(e.path()) {
                    Ok(bytes) => jobs.push((e.file_name().to_string_lossy().into_owned(), bytes)),
                    Err(_) => {
                        *by_error.entry("file read failed".into()).or_default() += 1;
                        failed += 1;
                    }
                }
            }
        }
    }

    for (label, data) in &jobs {
        if stand_dump {
            // What the character is standing on: every part whose XY footprint contains the
            // scene's own `collidee` anchor, nearest surface first. Names and centroids cannot
            // answer this — a 2000-unit ground sheet and a 172-unit plinth have the same centroid.
            let anchor = caer_assets::nif::read_skeleton(data)
                .ok()
                .and_then(|sk| {
                    sk.bones
                        .iter()
                        .find(|b| b.name.eq_ignore_ascii_case("collidee"))
                        .map(|b| caer_assets::nif::xform_to_mat4(&b.world_bind))
                })
                .map(|m| [m[3][0], m[3][1], m[3][2]]);
            let Some(anchor) = anchor else {
                println!("=== {label}  no `collidee` anchor ===");
                continue;
            };
            match caer_assets::nif::read_model(data) {
                Ok(m) => {
                    println!(
                        "=== {label}  anchor ({:.1}, {:.1}, {:.1}) ===",
                        anchor[0], anchor[1], anchor[2]
                    );
                    let mut hits: Vec<(f32, f32, f32, &str, &str, usize)> = Vec::new();
                    for p in &m.parts {
                        if p.positions.is_empty() {
                            continue;
                        }
                        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
                        for v in &p.positions {
                            for a in 0..3 {
                                lo[a] = lo[a].min(v[a]);
                                hi[a] = hi[a].max(v[a]);
                            }
                        }
                        if anchor[0] < lo[0]
                            || anchor[0] > hi[0]
                            || anchor[1] < lo[1]
                            || anchor[1] > hi[1]
                        {
                            continue;
                        }
                        let span = (hi[0] - lo[0]).max(hi[1] - lo[1]);
                        hits.push((
                            hi[2],
                            span,
                            hi[2] - anchor[2],
                            &p.name,
                            p.texture.as_deref().unwrap_or("<NONE>"),
                            p.positions.len(),
                        ));
                    }
                    hits.sort_by(|a, b| b.0.total_cmp(&a.0));
                    println!(
                        "{:<22} {:>8} {:>8} {:>8} {:>6}  {:<22} {:<22} vertex-alpha",
                        "part", "top z", "xy span", "dz feet", "verts", "layer 1", "layer 2"
                    );
                    for (top, span, dz, name, tex, nv) in hits.iter().take(12) {
                        let part = m
                            .parts
                            .iter()
                            .find(|p| &p.name == *name && p.positions.len() == *nv);
                        let (tex2, mask) = match part {
                            Some(p) => {
                                let a: Vec<f32> = p.colors.iter().map(|c| c[3]).collect();
                                let m = if a.is_empty() {
                                    "none".to_string()
                                } else {
                                    let lo = a.iter().copied().fold(f32::MAX, f32::min);
                                    let hi = a.iter().copied().fold(f32::MIN, f32::max);
                                    let mean = a.iter().sum::<f32>() / a.len() as f32;
                                    format!("{lo:.2}..{hi:.2} mean {mean:.2}")
                                };
                                (p.texture2.as_deref().unwrap_or("-").to_string(), m)
                            }
                            None => ("?".into(), "?".into()),
                        };
                        println!(
                            "{:<22} {:>8.1} {:>8.1} {:>8.1} {:>6}  {:<22} {:<22} {mask}",
                            trunc(name, 22),
                            top,
                            span,
                            dz,
                            nv,
                            trunc(tex, 22),
                            trunc(&tex2, 22)
                        );
                    }
                }
                Err(e) => println!("=== {label}  model failed: {e} ==="),
            }
            continue;
        }
        if emit_dump {
            match caer_assets::nif::read_particle_emitters(data) {
                Ok(es) => {
                    println!("=== {label}  {} particle emitters ===", es.len());
                    for e in &es {
                        println!(
                            "  {}{:<26} origin ({:.0},{:.0},{:.0})  rate {:.1}/s  life {:.1}s  size {:.1}  speed {:.1}  emit {:.1}..{:.1}",
                            if e.mesh_particles { "[mesh] " } else { "" },
                            trunc(&e.name, 26),
                            e.origin[0], e.origin[1], e.origin[2],
                            e.emit_rate, e.lifetime, e.size, e.speed,
                            e.emit_start, e.emit_stop
                        );
                        println!(
                            "      color rgba ({:.2},{:.2},{:.2},{:.2})  grow {:.2} fade {:.2}  decl {:.2} planar {:.2}",
                            e.color[0], e.color[1], e.color[2], e.color[3], e.grow, e.fade,
                            e.declination, e.planar_angle
                        );
                    }
                }
                Err(e) => println!("=== {label}  emitters failed: {e} ==="),
            }
            continue;
        }
        if uv_dump {
            match caer_assets::nif::uv_controllers(data) {
                Ok(cs) => {
                    println!(
                        "=== {label}  {} texture-transform controllers ===",
                        cs.len()
                    );
                    let types = caer_assets::nif::block_type_sequence(data).unwrap_or_default();
                    for (block, target, slot, op, keys) in &cs {
                        let what = usize::try_from(*target)
                            .ok()
                            .and_then(|t| types.get(t))
                            .map_or("<none>", String::as_str);
                        println!(
                            "  block {block:>5}  target {target:>5} ({what})  slot {slot}  op {op}  {keys} keys"
                        );
                    }
                }
                Err(e) => println!("=== {label}  uv failed: {e} ==="),
            }
            continue;
        }
        if stages_dump {
            match caer_assets::nif::texturing_stages(data) {
                Ok(stages) => {
                    let multi: Vec<_> = stages.iter().filter(|(_, _, s)| s.len() > 1).collect();
                    println!(
                        "=== {label}  {} texturing properties, {} with more than one stage ===",
                        stages.len(),
                        multi.len()
                    );
                    for (block, map_slots, slots) in &stages {
                        if slots.len() < 2 {
                            continue;
                        }
                        let cells: Vec<String> = slots
                            .iter()
                            .map(|(s, f)| {
                                let kind = if *s >= *map_slots { "shader" } else { "map" };
                                format!("[{kind} {s}] {f}")
                            })
                            .collect();
                        println!("  block {block:>4}  {}", cells.join("   "));
                    }
                }
                Err(e) => println!("=== {label}  stages failed: {e} ==="),
            }
            continue;
        }
        if morphs_dump || morph_detail {
            match caer_assets::nif::read_model(data) {
                Ok(m) => {
                    let morphed: Vec<_> = m.parts.iter().filter(|p| p.morph.is_some()).collect();
                    let timed: Vec<_> = morphed
                        .iter()
                        .copied()
                        .filter(|p| p.morph.as_ref().is_some_and(|m| m.is_animated()))
                        .collect();
                    let static_blends: Vec<_> = morphed
                        .iter()
                        .copied()
                        .filter(|p| {
                            p.morph
                                .as_ref()
                                .is_some_and(|m| !m.is_animated() && m.has_static_targets())
                        })
                        .collect();
                    let verts: usize = morphed.iter().map(|p| p.positions.len()).sum();
                    let dur = timed
                        .iter()
                        .filter_map(|p| p.morph.as_ref().map(caer_assets::nif::MorphAnim::duration))
                        .fold(0.0_f32, f32::max);
                    println!(
                        "=== {label}  {}/{} parts with morph data ({}/{} timed/static), {verts} morph vertices, longest timed clip {dur:.2}s ===",
                        morphed.len(),
                        m.parts.len()
                        ,timed.len(),
                        static_blends.len(),
                    );
                    let scrolling: Vec<_> =
                        m.parts.iter().filter(|p| p.uv_anim.is_some()).collect();
                    println!("  {} parts with animated UVs:", scrolling.len());
                    for p in &scrolling {
                        let a = p.uv_anim.as_ref().expect("filtered");
                        let ops: Vec<String> = a
                            .channels
                            .iter()
                            .map(|(o, k)| format!("{o:?}x{}", k.len()))
                            .collect();
                        println!(
                            "    {:<24} {:>5}v  {:.2}s  {}  tex={}",
                            trunc(&p.name, 24),
                            p.positions.len(),
                            a.duration(),
                            ops.join(" "),
                            p.texture.as_deref().unwrap_or("-")
                        );
                    }
                    for p in morphed.iter().take(8) {
                        let a = p.morph.as_ref().expect("filtered");
                        let keyed = a.targets.iter().filter(|t| t.keys.len() > 1).count();
                        println!(
                            "  {:<28} {:>4}v  {} targets ({keyed} keyed)  {:.2}s  timed={} static={} billboard={}",
                            p.name,
                            p.positions.len(),
                            a.targets.len(),
                            a.duration(),
                            a.is_animated(),
                            a.has_static_targets(),
                            p.billboard
                        );
                    }
                    if morph_detail {
                        for p in &morphed {
                            let a = p.morph.as_ref().expect("filtered");
                            println!(
                                "  detail {}  relative={}  {} targets",
                                p.name,
                                a.relative,
                                a.targets.len()
                            );
                            for (index, target) in a.targets.iter().enumerate() {
                                // Static character heads encode their facial channels as one
                                // controller with a base target plus twelve sparse deltas. The
                                // NIF's UserPropBufferY maps a target to an `FM` id; include both
                                // that source tag and the affected geometry so this existing
                                // inspector, not a second figure-only binary, proves a mapping.
                                let mut active = 0usize;
                                let mut center = [0.0_f32; 3];
                                let mut lo = [f32::INFINITY; 3];
                                let mut hi = [f32::NEG_INFINITY; 3];
                                let max_delta = target
                                    .deltas
                                    .iter()
                                    .enumerate()
                                    .map(|(vertex, delta)| {
                                        let magnitude = (delta[0] * delta[0]
                                            + delta[1] * delta[1]
                                            + delta[2] * delta[2])
                                            .sqrt();
                                        if magnitude > 1.0e-5 {
                                            active += 1;
                                            let position = p
                                                .positions
                                                .get(vertex)
                                                .copied()
                                                .unwrap_or([0.0; 3]);
                                            for axis in 0..3 {
                                                center[axis] += position[axis];
                                                lo[axis] = lo[axis].min(position[axis]);
                                                hi[axis] = hi[axis].max(position[axis]);
                                            }
                                        }
                                        magnitude
                                    })
                                    .fold(0.0_f32, f32::max);
                                if active > 0 {
                                    for axis in 0..3 {
                                        center[axis] /= active as f32;
                                    }
                                }
                                let keys = target
                                    .keys
                                    .iter()
                                    .map(|key| format!("{:.9}:{:.6}", key.time, key.value[0]))
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                let source_tag = p
                                    .morph_target_ids
                                    .get(index)
                                    .copied()
                                    .flatten()
                                    .map_or_else(|| "-".to_string(), |id| format!("FM{id}"));
                                println!(
                                    "    target {index:>2} ({source_tag:>4}): {:>3} keys [{}]  {:>5} deltas  {active:>4} active  max |Δ| {max_delta:.5}  center=({:.2},{:.2},{:.2}) span=({:.2}..{:.2},{:.2}..{:.2},{:.2}..{:.2})",
                                    target.keys.len(),
                                    keys,
                                    target.deltas.len(),
                                    center[0],
                                    center[1],
                                    center[2],
                                    lo[0],
                                    hi[0],
                                    lo[1],
                                    hi[1],
                                    lo[2],
                                    hi[2],
                                );
                            }
                        }
                    }
                }
                Err(e) => println!("=== {label}  model failed: {e} ==="),
            }
            continue;
        }
        if blocks_dump {
            match caer_assets::nif::read_header(data) {
                Ok(h) => {
                    let mut counts: HashMap<&str, usize> = HashMap::new();
                    for t in &h.block_types {
                        *counts.entry(t.as_str()).or_default() += 1;
                    }
                    let mut rows: Vec<_> = counts.into_iter().collect();
                    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
                    println!("=== {label}  {} block types ===", rows.len());
                    for (t, n) in rows {
                        println!("  {n:5}  {t}");
                    }
                }
                Err(e) => println!("=== {label}  header failed: {e} ==="),
            }
            continue;
        }
        if doors_dump {
            match caer_assets::nif::read_door_anchors(data) {
                Ok(anchors) => {
                    println!("=== {label}  {} door anchors ===", anchors.len());
                    println!(
                        "{:<32} {:>6} {:>10} {:>10} {:>10} {:>10} {:>10}",
                        "name", "kind", "tx", "ty", "tz", "m(y,x)°", "m(-y,x)°"
                    );
                    for a in &anchors {
                        let [tx, ty, tz] = a.translation;
                        let m_yx = ty.atan2(tx).to_degrees();
                        let m_nyx = (-ty).atan2(tx).to_degrees();
                        println!(
                            "{:<32} {:>6} {:>10.2} {:>10.2} {:>10.2} {:>10.2} {:>10.2}",
                            trunc(&a.name, 32),
                            a.kind,
                            tx,
                            ty,
                            tz,
                            m_yx,
                            m_nyx
                        );
                    }
                    println!();
                }
                Err(e) => eprintln!("door-anchors {label}: {e}"),
            }
        }
        match caer_assets::nif::read_model(data) {
            Ok(model) => {
                ok += 1;
                parts += model.parts.len();
                tris += model
                    .parts
                    .iter()
                    .map(|p| p.indices.len() / 3)
                    .sum::<usize>();
                if parts_dump {
                    println!("=== {label}  {} parts ===", model.parts.len());
                    println!(
                        "{:<32} {:>8} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
                        "name", "verts", "cx", "cy", "cz", "m(y,x)°", "texture", "alpha"
                    );
                    for p in &model.parts {
                        let (cx, cy, cz, n) = centroid(&p.positions);
                        let (m_yx, m_nyx) = if n > 0 && (cx * cx + cy * cy) > 1e-6 {
                            (cy.atan2(cx).to_degrees(), (-cy).atan2(cx).to_degrees())
                        } else {
                            (f32::NAN, f32::NAN)
                        };
                        let door = if p.name.to_ascii_lowercase().contains("door") {
                            "DOOR"
                        } else {
                            ""
                        };
                        let _ = (m_nyx, door);
                        println!(
                            "{:<32} {:>8} {:>10.2} {:>10.2} {:>10.2} {:>10.2}  {:<28} {:?}",
                            trunc(&p.name, 32),
                            p.positions.len(),
                            cx,
                            cy,
                            cz,
                            m_yx,
                            p.texture.as_deref().unwrap_or("<NONE>"),
                            p.alpha
                        );
                    }
                    println!();
                }
                // Verbose: print the model's raw-unit bounding box. This is the calibration
                // surface for the fixture scale constant — compare against the Ref Height /
                // Ref Width columns in each zone's nifs.csv (intended world-unit size).
                if verbose {
                    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
                    for p in &model.parts {
                        for v in &p.positions {
                            for a in 0..3 {
                                lo[a] = lo[a].min(v[a]);
                                hi[a] = hi[a].max(v[a]);
                            }
                        }
                    }
                    let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
                    println!(
                        "OK   {}: size [{:.1}, {:.1}, {:.1}] lo [{:.1}, {:.1}, {:.1}]",
                        label, size[0], size[1], size[2], lo[0], lo[1], lo[2]
                    );
                }
            }
            Err(err) => {
                failed += 1;
                // bucket by the error's leading words so layout gaps group together
                let key = err.to_string();
                let key = key.split(" (").next().unwrap_or(&key).to_string();
                *by_error.entry(key).or_default() += 1;
                if verbose {
                    eprintln!("FAIL {label}: {err}");
                }
            }
        }
    }

    let total = ok + failed;
    println!(
        "nifstat: {ok}/{total} models parsed ({:.1}%), {parts} mesh parts, {tris} triangles",
        100.0 * ok as f64 / total.max(1) as f64
    );
    let mut errs: Vec<_> = by_error.into_iter().collect();
    errs.sort_by_key(|b| std::cmp::Reverse(b.1));
    for (msg, n) in errs.into_iter().take(15) {
        println!("  {n:5}  {msg}");
    }
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    if let Some(i) = args.iter().position(|a| a == flag) {
        args.remove(i);
        true
    } else {
        false
    }
}

fn centroid(positions: &[[f32; 3]]) -> (f32, f32, f32, usize) {
    let n = positions.len();
    if n == 0 {
        return (0.0, 0.0, 0.0, 0);
    }
    let mut sx = 0.0f64;
    let mut sy = 0.0f64;
    let mut sz = 0.0f64;
    for v in positions {
        sx += f64::from(v[0]);
        sy += f64::from(v[1]);
        sz += f64::from(v[2]);
    }
    let inv = 1.0 / n as f64;
    ((sx * inv) as f32, (sy * inv) as f32, (sz * inv) as f32, n)
}

fn trunc(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n.saturating_sub(1)])
    }
}
