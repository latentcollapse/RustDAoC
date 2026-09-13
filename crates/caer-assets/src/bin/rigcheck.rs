//! `rigcheck` — verify skeletal extraction (Phase A.3.1) against real figure meshes.
//!
//! For each `figures/*.nif`, extracts the rigged skeleton + skin ([`nif::read_rigged`]) and
//! reconstructs the bind pose (`world_bind · inverse_bind · v`), comparing it to the static
//! ([`nif::read_model`]) geometry. A near-zero error means bones, weights, and inverse-binds are
//! captured correctly. A large *constant* offset flags the shape-node-transform quirk (the static
//! flatten bakes a NiTriShape's own translation that the skin correctly ignores — there the rigged
//! bind is the truer one).
//!
//!   rigcheck <figures-dir> [name-substr]     # e.g. rigcheck .../figures skel

use caer_assets::nif;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(dir) = args.next() else {
        eprintln!("usage: rigcheck <figures-dir> [name-substr]");
        std::process::exit(2);
    };
    let filter = args.next().unwrap_or_default().to_ascii_lowercase();

    let (mut rigged, mut clean, mut offset, mut failed) = (0, 0, 0, 0);
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| {
            eprintln!("rigcheck: {dir}: {e}");
            std::process::exit(1);
        })
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();

    for path in entries {
        if !path
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("nif"))
        {
            continue;
        }
        let stem = path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_ascii_lowercase();
        if !filter.is_empty() && !stem.contains(&filter) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let rig = match nif::read_rigged(&bytes) {
            Ok(Some(r)) => r,
            Ok(None) => continue, // static prop, not rigged
            Err(_) => {
                failed += 1;
                continue;
            }
        };
        rigged += 1;
        let Ok(stat) = nif::read_model(&bytes) else {
            failed += 1;
            continue;
        };

        let (mut max, mut sum, mut n) = (0f32, 0f32, 0usize);
        for (pi, rp) in rig.parts.iter().enumerate() {
            let Some(sp) = stat
                .parts
                .iter()
                .find(|s| s.positions.len() == rp.positions.len())
            else {
                continue;
            };
            for vi in 0..rp.positions.len() {
                let b = rig.bind_position(pi, vi);
                let s = sp.positions[vi];
                let d =
                    ((b[0] - s[0]).powi(2) + (b[1] - s[1]).powi(2) + (b[2] - s[2]).powi(2)).sqrt();
                max = max.max(d);
                sum += d;
                n += 1;
            }
        }
        let mean = sum / n.max(1) as f32;
        // Near-zero → clean; a large near-constant offset → the shape-transform quirk.
        let tag = if max < 1.0 {
            clean += 1;
            "ok"
        } else if (max - mean).abs() < mean * 0.2 {
            offset += 1;
            "OFFSET"
        } else {
            "err"
        };
        if !filter.is_empty() || tag != "ok" {
            println!(
                "{:<28} {} bones {} parts  mean={:.3} max={:.3}  [{tag}]",
                stem,
                rig.skeleton.bones.len(),
                rig.parts.len(),
                mean,
                max
            );
        }
    }
    println!("--- rigged={rigged} clean={clean} shape-offset={offset} parse-failed={failed}");
}
