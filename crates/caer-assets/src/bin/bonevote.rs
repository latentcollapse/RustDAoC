//! `bonevote` — derive the canonical Biped id→name table empirically (Phase A.3.2b).
//!
//! Every humanoid clip shares one canonical id→bone semantics. A bone's bind LOCAL translation
//! (offset from its parent) is constant across clips and distinctive per bone, so matching each
//! clip track's t=0 translation to the nearest bone of a rich reference biped recovers id→name.
//! Voting across every humanoid clip (and discarding poor matches) washes out per-clip noise and
//! auto-rejects non-humanoid creature clips (their translations don't fit the biped reference).
//!
//! L/R-symmetric bones share a local translation, so their side may be assigned by reference order;
//! that is cosmetic for a symmetric idle and is corrected structurally where it matters (later).
//!
//!   bonevote <reference-biped.nif> <anims-dir>   [max_id]

use caer_assets::nif;
use std::collections::HashMap;

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let refm = args.next().expect("reference biped.nif");
    let dir = args.next().expect("anims dir");
    let max_id: i32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(80);

    let skel = nif::read_skeleton(&std::fs::read(&refm).unwrap()).unwrap();
    let pool: Vec<(&str, [f32; 3])> = skel
        .bones
        .iter()
        .filter(|b| b.name == "Bip01" || b.name.starts_with("Bip01 "))
        .map(|b| (b.name.as_str(), b.local.2))
        .collect();

    // id -> name -> accumulated confidence weight
    let mut votes: HashMap<i32, HashMap<String, f32>> = HashMap::new();
    let (mut clips_used, mut clips_skipped) = (0u32, 0u32);

    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("kfa")))
        .collect();
    paths.sort();
    for p in &paths {
        let Ok(bytes) = std::fs::read(p) else {
            continue;
        };
        let Ok(clip) = nif::read_clip(&bytes) else {
            continue;
        };
        if clip.tracks.keys().copied().max().unwrap_or(0) > max_id {
            clips_skipped += 1;
            continue;
        }
        // Score this clip's overall fit; skip clips that match the biped reference poorly.
        let mut matches: Vec<(i32, &str, f32)> = Vec::new();
        let mut resid = 0.0f32;
        let mut ntr = 0u32;
        for (&id, tr) in &clip.tracks {
            let Some(t0) = tr.translation.first().map(|k| k.value) else {
                continue;
            };
            let mut best = (f32::MAX, "");
            let mut second = f32::MAX;
            for &(name, bt) in &pool {
                let d = dist(t0, bt);
                if d < best.0 {
                    second = best.0;
                    best = (d, name);
                } else if d < second {
                    second = d;
                }
            }
            if best.1.is_empty() {
                continue;
            }
            resid += best.0;
            ntr += 1;
            matches.push((id, best.1, second - best.0));
        }
        if ntr == 0 || resid / ntr as f32 > 0.10 {
            clips_skipped += 1;
            continue;
        } // only exact-fit clips vote
        clips_used += 1;
        for (id, name, margin) in matches {
            // Weight: confident (large-margin) matches count more; ties still contribute a little.
            let w = 1.0 + margin.min(20.0);
            *votes
                .entry(id)
                .or_default()
                .entry(name.to_string())
                .or_default() += w;
        }
    }

    println!("reference: {refm}  ({} biped pool bones)", pool.len());
    println!("clips: {clips_used} used, {clips_skipped} skipped (non-biped / poor fit)\n");
    println!("// Canonical Biped id -> name (empirically voted from {clips_used} clips)");
    let maxk = votes.keys().copied().max().unwrap_or(0);
    for id in 0..=maxk {
        let Some(m) = votes.get(&id) else {
            println!("  {id:>3}  <no votes>");
            continue;
        };
        let total: f32 = m.values().sum();
        let mut ranked: Vec<_> = m.iter().collect();
        ranked.sort_by(|a, b| b.1.total_cmp(a.1));
        let (win, wv) = ranked[0];
        let agree = 100.0 * wv / total;
        let runner = ranked
            .get(1)
            .map(|(n, v)| format!("  (2nd: {} {:.0}%)", n, 100.0 * *v / total))
            .unwrap_or_default();
        println!("  {id:>3}  {win:<22} {agree:>3.0}%{runner}");
    }
}
