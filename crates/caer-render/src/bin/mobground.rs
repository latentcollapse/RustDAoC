//! `mobground` — measure how far each spawn's AUTHORED z sits from our decoded terrain surface.
//!
//! The renderer currently discards the server's z outright and snaps every entity to
//! `height_at(x, y)`. That was a reasonable first cut — an ungrounded mob floats or sinks wherever
//! our terrain decode disagrees with the server's — but it is only correct if the two genuinely
//! agree almost everywhere. Where they don't, snapping is not a fix but a second error laid over
//! the first, and it actively destroys the cases where the authored z is RIGHT and off-surface by
//! design: anything flying, on an upper floor, on a bridge, or on a wall.
//!
//! So measure before choosing a policy. This reports the distribution of `authored_z − surface_z`
//! over a whole region's spawn table, which is what decides whether to trust the packet or the
//! heightfield — and at what threshold.
//!
//! Usage: `mobground <mobs.tsv> [--region N]`

use caer_render::terrain;
use glam::Vec3;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: mobground <mobs.tsv> [--region N]");
        std::process::exit(2);
    };
    let mut region: u16 = 1;
    while let Some(a) = args.next() {
        if a == "--region" {
            region = args.next().and_then(|v| v.parse().ok()).unwrap_or(1);
        }
    }

    // Load the spawn table and the region's terrain over the spawns' own bounding box, so every
    // sampled point is on loaded ground rather than off the edge of an arbitrary window.
    let world = caer_render::load_world(&path);
    let mut rows: Vec<([i32; 3], u16)> = Vec::new();
    for v in &world {
        rows.push((v.pos, v.model));
    }
    if rows.is_empty() {
        eprintln!("mobground: no spawns in {path}");
        std::process::exit(1);
    }
    let (mut min, mut max) = ([i32::MAX; 2], [i32::MIN; 2]);
    for (p, _) in &rows {
        for k in 0..2 {
            min[k] = min[k].min(p[k]);
            max[k] = max[k].max(p[k]);
        }
    }
    eprintln!(
        "mobground: {} spawns, xy {min:?}..{max:?}, loading region {region} terrain…",
        rows.len()
    );
    let mesh = terrain::load_region(region, Vec3::ZERO, min, max, terrain::seam_blend());

    // delta = authored z - our surface z. Positive means the spawn sits ABOVE our ground.
    let mut deltas: Vec<f32> = Vec::with_capacity(rows.len());
    let mut off_terrain = 0usize;
    for (p, _) in &rows {
        match mesh.height_at(p[0], p[1]) {
            Some(h) => deltas.push(p[2] as f32 - h),
            None => off_terrain += 1,
        }
    }
    if deltas.is_empty() {
        eprintln!("mobground: no spawn landed on loaded terrain");
        std::process::exit(1);
    }
    deltas.sort_by(f32::total_cmp);

    let pct = |q: f32| deltas[((deltas.len() - 1) as f32 * q) as usize];
    let within = |t: f32| deltas.iter().filter(|d| d.abs() <= t).count();
    let n = deltas.len() as f32;

    println!(
        "sampled {} spawns ({off_terrain} off loaded terrain)",
        deltas.len()
    );
    println!("authored_z - surface_z:");
    println!("  min {:.0}  p01 {:.0}  p05 {:.0}  p25 {:.0}  median {:.0}  p75 {:.0}  p95 {:.0}  p99 {:.0}  max {:.0}",
        deltas[0], pct(0.01), pct(0.05), pct(0.25), pct(0.50), pct(0.75), pct(0.95), pct(0.99), deltas[deltas.len() - 1]);
    for t in [1.0f32, 4.0, 16.0, 64.0, 128.0, 256.0, 1024.0] {
        println!(
            "  within +/-{t:>6.0}: {:>6} ({:>5.1}%)",
            within(t),
            100.0 * within(t) as f32 / n
        );
    }
    // Name the worst offenders so the elevated cases can actually be looked at in the renderer —
    // a percentage tells you the policy is wrong, a coordinate tells you where to point the camera.
    let mut elevated: Vec<([i32; 3], u16, f32)> = rows
        .iter()
        .filter_map(|(p, m)| {
            mesh.height_at(p[0], p[1])
                .map(|h| (*p, *m, p[2] as f32 - h))
        })
        .filter(|(_, _, d)| *d > 256.0)
        .collect();
    elevated.sort_by(|a, b| b.2.total_cmp(&a.2));
    println!("most-elevated spawns (authored z above our surface):");
    for (p, model, d) in elevated.iter().take(8) {
        println!(
            "  +{d:>6.0}  model {model:>5}  at {},{},{}",
            p[0], p[1], p[2]
        );
    }

    let above = deltas.iter().filter(|&&d| d > 256.0).count();
    let below = deltas.iter().filter(|&&d| d < -256.0).count();
    println!(
        "  far ABOVE surface (>256): {above} ({:.1}%)   far BELOW (<-256): {below} ({:.1}%)",
        100.0 * above as f32 / n,
        100.0 * below as f32 / n
    );
}
