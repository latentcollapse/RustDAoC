//! `parity_audit` — Layer 1 of the client-parity harness: static analyzers over the DAoC
//! client's zone data that flag inconsistency candidates WITHOUT needing the client running.
//!
//! Each finding is one TSV row on stdout (machine-consumable — the parity queue); a human
//! summary goes to stderr. Checks:
//!
//!   * `zone-no-grid-offset`  — a zoneNNN dir on disk whose id has no offset in the Zones table
//!     (renders as a hole; the region-51 city/Galladoria bucket).
//!   * `fixture-out-of-zone`  — a placement whose zone-local X/Y falls outside the 65,536-unit
//!     zone square (lands in a neighbouring zone or nowhere).
//!   * `fixture-at-origin`    — a placement at exactly local (0,0,0). Legit when the model is a
//!     baked-coordinate quadrant mesh (verts carry zone coords, so its
//!     bbox sits far from the model origin); SUSPECT when the model's
//!     bbox hugs the origin — that object is really stacked at the
//!     zone corner.
//!   * `water-floating` / `water-buried` — a SECTOR.DAT water body whose surface height is far
//!     above the terrain under EVERY bank point (the Aldland floating
//!     strip) or far below it (invisible, z-fight fodder).
//!   * `nif-*`                — every placement joined against its model's parse status: which
//!     placements point at a failing NIF (bucketed by walker error),
//!     a missing .npk, an authentic empty placeholder, or a model that
//!     only exists in `Dnifs/` (the renderer indexes `Nifs/` only).
//!
//! The `nif-*` aggregation is the ranking tool for walker work: the stderr summary totals
//! placements per error bucket, so "Gamebryo 10.1 blocks N placements across M zones" is a
//! number, not a hunch.
//!
//!   parity_audit [--region N] [--origin-detail]
//!
//! Client data root: `$CAER_CLIENT/zones`, else the default Wine install path.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use caer_world::{zone_grid_offset, zone_name, zone_region};

/// A water surface this far above/below the terrain under all of its bank points is flagged.
/// Banks sit at roughly water level in real data; 700 units (~3 character heights) is far past
/// any legitimate embankment.
const WATER_ABSURD: f32 = 700.0;
/// An origin-placed fixture whose model bbox starts this far from the model origin is a
/// baked-coordinate quadrant mesh (zone coords live in the verts) — expected, not a bug.
const BAKED_MIN: f32 = 4096.0;
/// Zone side length in world units (8 grid cells × 8,192).
const ZONE_SIDE: f32 = 65_536.0;

/// Parse status of one model stem, cached across all zones (palettes share models heavily).
enum NifStatus {
    /// Parsed; carries the mesh bbox min so the origin-fixture check can classify baked coords.
    Ok { bbox_min: [f32; 3] },
    /// Walker error, bucketed by its leading words (same bucketing as nifstat).
    ParseFail(String),
    /// The .nif member is an authentic tiny placeholder (~261 bytes) — correct to skip.
    EmptyPlaceholder,
    /// No .npk for this stem in `Nifs/` at all.
    MissingNpk,
    /// Absent from `Nifs/` but present in `Dnifs/` — the renderer never looks there.
    OnlyInDnifs,
}

fn client_root() -> PathBuf {
    if let Ok(root) = std::env::var("CAER_CLIENT") {
        return PathBuf::from(root);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".wine/drive_c/Program Files (x86)/RustDAoC")
}

/// The client's FOUR parallel zone roots (overworld, New Frontiers, housing, tutorial) — same
/// per-zone mpk structure in each; the renderer scans all of them, so the audit must too.
fn zone_roots() -> Vec<PathBuf> {
    let root = client_root();
    [
        "zones",
        "frontiers/zones",
        "phousing/zones",
        "Tutorial/zones",
    ]
    .iter()
    .map(|s| root.join(s))
    .collect()
}

/// Model dirs in the renderer's priority order (first hit wins on a stem clash).
fn model_dirs() -> Vec<PathBuf> {
    let root = client_root();
    [
        "zones/Nifs",
        "zones/trees",
        "zones/Dnifs",
        "frontiers/NIFS",
        "frontiers/dnifs",
        "phousing/nifs",
    ]
    .iter()
    .map(|s| root.join(s))
    .collect()
}

/// Lower-cased stem → path index of every `.npk` in a dir (case-insensitive on purpose:
/// palettes cite mixed-case names, Linux is case-sensitive).
fn npk_index(dir: &Path) -> HashMap<String, PathBuf> {
    let mut out = HashMap::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if let Some(stem) = name.strip_suffix(".npk") {
                out.insert(stem.to_string(), e.path());
            }
        }
    }
    out
}

/// Resolve one model stem to its parse status (called once per stem, memoised by the caller).
fn nif_status(
    stem: &str,
    nifs: &HashMap<String, PathBuf>,
    dnifs: &HashMap<String, PathBuf>,
) -> NifStatus {
    let Some(path) = nifs.get(stem) else {
        return if dnifs.contains_key(stem) {
            NifStatus::OnlyInDnifs
        } else {
            NifStatus::MissingNpk
        };
    };
    let Ok(members) = caer_assets::open(path) else {
        return NifStatus::ParseFail("mpak read failed".into());
    };
    let Some(nif) = members
        .iter()
        .find(|m| m.name.to_ascii_lowercase().ends_with(".nif"))
    else {
        return NifStatus::ParseFail("no .nif member".into());
    };
    // The client ships authentic ~261-byte stub NIFs for retired models; skipping them is
    // correct behaviour, but placements pointing at one are still worth a row.
    if nif.data.len() < 300 {
        return NifStatus::EmptyPlaceholder;
    }
    match caer_assets::nif::read_model(&nif.data) {
        Ok(model) => {
            let mut lo = [f32::MAX; 3];
            for p in &model.parts {
                for v in &p.positions {
                    for a in 0..3 {
                        lo[a] = lo[a].min(v[a]);
                    }
                }
            }
            NifStatus::Ok { bbox_min: lo }
        }
        Err(err) => {
            // Bucket by the error's leading words so layout gaps group (nifstat's convention).
            let key = err.to_string();
            NifStatus::ParseFail(key.split(" (").next().unwrap_or(&key).to_string())
        }
    }
}

fn main() {
    let mut region_filter: Option<u16> = None;
    let mut origin_detail = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--region" => region_filter = it.next().and_then(|s| s.parse().ok()),
            "--origin-detail" => origin_detail = true,
            other => {
                eprintln!(
                    "usage: parity_audit [--region N] [--origin-detail]  (unknown arg {other:?})"
                );
                std::process::exit(2);
            }
        }
    }

    // Model index across every dir the renderer scans, in the same priority order.
    let mut nifs: HashMap<String, PathBuf> = HashMap::new();
    for dir in model_dirs() {
        for (stem, path) in npk_index(&dir) {
            nifs.entry(stem).or_insert(path);
        }
    }
    let dnifs: HashMap<String, PathBuf> = HashMap::new(); // Dnifs is in the main index now

    let mut zones: Vec<(u16, PathBuf)> = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();
    let mut any_root = false;
    for dir in zone_roots() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        any_root = true;
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if let Some(id) = name
                .strip_prefix("zone")
                .and_then(|s| s.parse::<u16>().ok())
            {
                if seen_ids.insert(id) {
                    zones.push((id, e.path()));
                }
            }
        }
    }
    if !any_root {
        eprintln!(
            "parity_audit: no client zones dir under {}",
            client_root().display()
        );
        std::process::exit(1);
    }
    zones.sort_by_key(|z| z.0);

    let out = std::io::stdout();
    let mut out = out.lock();
    writeln!(
        out,
        "check\tregion\tzone\tzone_name\tsubject\tdetail\tcount"
    )
    .unwrap();
    // One row = one finding. `subject` is the thing (fixture id, model stem, water-body name),
    // `detail` the evidence, `count` how many placements it covers (1 unless aggregated).
    let mut row =
        |check: &str, region: u16, zone: u16, subject: &str, detail: &str, count: usize| {
            let name = zone_name(zone).unwrap_or("?");
            writeln!(
                out,
                "{check}\t{region}\t{zone}\t{name}\t{subject}\t{detail}\t{count}"
            )
            .unwrap();
        };

    // Global caches/aggregates across zones.
    let mut model_cache: HashMap<String, NifStatus> = HashMap::new();
    // error bucket -> (total placements, distinct stems) — the walker-work ranking.
    let mut bucket_totals: HashMap<String, (usize, std::collections::HashSet<String>)> =
        HashMap::new();
    let mut totals: HashMap<&'static str, usize> = HashMap::new();
    let mut zones_scanned = 0usize;
    let mut fixtures_scanned = 0usize;

    for (id, path) in zones {
        let region = zone_region(id);
        match (region, region_filter) {
            // Unmapped dirs get reported regardless of the filter — their region is unknowable.
            (None, _) => {
                row(
                    "zone-no-grid-offset",
                    0,
                    id,
                    "-",
                    "zone dir exists on disk but id has no Zones-table offset",
                    1,
                );
                *totals.entry("zone-no-grid-offset").or_default() += 1;
                continue;
            }
            (Some(r), Some(want)) if r != want => continue,
            _ => {}
        }
        let region = region.unwrap();
        if zone_grid_offset(id).is_none() {
            continue; // can't happen when region resolved, but stay defensive
        }
        zones_scanned += 1;

        // ---- fixture checks (csvNNN.mpk) ----------------------------------------------------
        let fixtures = std::fs::read(path.join(format!("csv{id:03}.mpk")))
            .ok()
            .and_then(|b| caer_assets::fixtures::fixtures_from_csv_mpk(&b).ok())
            .unwrap_or_default();
        fixtures_scanned += fixtures.len();

        // Per-zone aggregation of NIF statuses: (stem, bucket) -> placement count.
        let mut zone_nif: HashMap<(String, String), usize> = HashMap::new();

        for f in &fixtures {
            // Out-of-zone: local coords must land inside the 65,536-unit square.
            if f.x < 0.0 || f.x >= ZONE_SIDE || f.y < 0.0 || f.y >= ZONE_SIDE {
                row(
                    "fixture-out-of-zone",
                    region,
                    id,
                    &format!("fixture {} ({})", f.id, f.name),
                    &format!("local ({:.0}, {:.0}, {:.0})", f.x, f.y, f.z),
                    1,
                );
                *totals.entry("fixture-out-of-zone").or_default() += 1;
            }

            // Resolve the model status once per stem, shared across all zones.
            let stem = f.filename.to_ascii_lowercase();
            let stem = stem.strip_suffix(".nif").unwrap_or(&stem).to_string();
            if stem.is_empty() {
                *zone_nif
                    .entry(("-".into(), "no-palette-entry".into()))
                    .or_default() += 1;
                continue;
            }
            let status = model_cache
                .entry(stem.clone())
                .or_insert_with(|| nif_status(&stem, &nifs, &dnifs));

            // Origin-placed: classify baked-coordinate mesh vs genuinely-at-the-corner.
            if f.x == 0.0 && f.y == 0.0 && f.z == 0.0 {
                match status {
                    NifStatus::Ok { bbox_min } => {
                        let far = bbox_min[0].abs().max(bbox_min[1].abs());
                        if far >= BAKED_MIN {
                            // Expected: quadrant mesh with zone coords baked into the verts.
                            if origin_detail {
                                row(
                                    "fixture-at-origin-baked",
                                    region,
                                    id,
                                    &format!("fixture {} ({stem})", f.id),
                                    &format!("bbox min offset {far:.0}u — baked coords, OK"),
                                    1,
                                );
                            }
                        } else {
                            row(
                                "fixture-at-origin-suspect",
                                region,
                                id,
                                &format!("fixture {} ({stem})", f.id),
                                &format!("model bbox starts {far:.0}u from origin — really stacked at zone corner"),
                                1,
                            );
                            *totals.entry("fixture-at-origin-suspect").or_default() += 1;
                        }
                    }
                    // Unparseable model: can't classify — report so it isn't silently missed.
                    _ => {
                        row(
                            "fixture-at-origin-unclassified",
                            region,
                            id,
                            &format!("fixture {} ({stem})", f.id),
                            "model unparseable, baked-vs-suspect unknown",
                            1,
                        );
                        *totals.entry("fixture-at-origin-unclassified").or_default() += 1;
                    }
                }
            }

            // NIF join: aggregate every non-OK placement per (stem, bucket).
            let bucket: Option<String> = match status {
                NifStatus::Ok { .. } => None,
                NifStatus::ParseFail(e) => Some(e.clone()),
                NifStatus::EmptyPlaceholder => Some("empty-placeholder".into()),
                NifStatus::MissingNpk => Some("missing-npk".into()),
                NifStatus::OnlyInDnifs => Some("only-in-dnifs".into()),
            };
            if let Some(b) = bucket {
                *zone_nif.entry((stem, b)).or_default() += 1;
            }
        }

        // Emit the zone's NIF findings, largest impact first.
        let mut nif_rows: Vec<_> = zone_nif.into_iter().collect();
        nif_rows.sort_by_key(|b| std::cmp::Reverse(b.1));
        for ((stem, bucket), n) in nif_rows {
            row(
                &format!("nif-{}", slug(&bucket)),
                region,
                id,
                &stem,
                &bucket,
                n,
            );
            let e = bucket_totals.entry(bucket).or_default();
            e.0 += n;
            e.1.insert(stem);
        }

        // ---- water checks (SECTOR.DAT in datNNN.mpk) ----------------------------------------
        let Ok(dat) = std::fs::read(path.join(format!("dat{id:03}.mpk"))) else {
            continue;
        };
        let Ok(terrain) = caer_assets::terrain::ZoneTerrain::from_dat_mpk(&dat) else {
            continue;
        };
        for body in caer_assets::sector::water_from_dat_mpk(&dat).unwrap_or_default() {
            // Terrain height under every bank point (bank coords are zone-local world units).
            let mut lo = f32::MAX;
            let mut hi = f32::MIN;
            for p in body.left.iter().chain(body.right.iter()) {
                let tx = (p[0] / caer_assets::terrain::SAMPLE_UNITS) as usize;
                let ty = (p[1] / caer_assets::terrain::SAMPLE_UNITS) as usize;
                let h = terrain.height(tx, ty);
                lo = lo.min(h);
                hi = hi.max(h);
            }
            let subject = if body.name.is_empty() {
                "(unnamed)"
            } else {
                &body.name
            };
            if body.height > hi + WATER_ABSURD {
                // Surface far above the terrain under EVERY bank point → visibly floating.
                row(
                    "water-floating",
                    region,
                    id,
                    subject,
                    &format!(
                        "surface {:.0} vs bank terrain {:.0}..{:.0}",
                        body.height, lo, hi
                    ),
                    1,
                );
                *totals.entry("water-floating").or_default() += 1;
            } else if body.height < lo - WATER_ABSURD {
                row(
                    "water-buried",
                    region,
                    id,
                    subject,
                    &format!(
                        "surface {:.0} vs bank terrain {:.0}..{:.0}",
                        body.height, lo, hi
                    ),
                    1,
                );
                *totals.entry("water-buried").or_default() += 1;
            }
        }
    }

    // ---- summary (stderr, so stdout stays a clean TSV) --------------------------------------
    eprintln!("parity_audit: {zones_scanned} zones, {fixtures_scanned} placements scanned");
    let mut t: Vec<_> = totals.into_iter().collect();
    t.sort_by_key(|b| std::cmp::Reverse(b.1));
    for (check, n) in t {
        eprintln!("  {n:6}  {check}");
    }
    eprintln!("parity_audit: placements blocked per NIF bucket (the walker-work ranking):");
    let mut b: Vec<_> = bucket_totals.into_iter().collect();
    b.sort_by_key(|y| std::cmp::Reverse(y.1 .0));
    for (bucket, (placements, stems)) in b {
        eprintln!(
            "  {placements:6} placements / {:4} models  {bucket}",
            stems.len()
        );
    }
}

/// Error bucket → short kebab slug for the TSV `check` column.
fn slug(bucket: &str) -> String {
    let s: String = bucket
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    // collapse runs of '-' and trim, keep it short
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars().take(40) {
        if c == '-' {
            if !dash && !out.is_empty() {
                out.push('-');
            }
            dash = true;
        } else {
            out.push(c);
            dash = false;
        }
    }
    out.trim_end_matches('-').to_string()
}
