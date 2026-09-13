//! How often does map slot 0 disagree with shader texture 0, across the whole client?
//!
//! Diagnostic. Run with `--nocapture`, scoped by `CAER_CENSUS_DIRS` (default: figures).
//!
//! Exists because the pre-world scenes turned out to bind the wrong base texture on 5 parts — a
//! prop atlas on Midgard's ground, rock on Albion's grass, moss on Hibernia's trunks — and that was
//! only ever found by a human looking at the screen. The same disagreement is mechanically
//! detectable, so the question worth answering is how much else was silently wrong.

use std::path::{Path, PathBuf};

fn client_root() -> PathBuf {
    caer_assets::client_dep::required_caer_client_root("texture_stage_census")
}

fn archives_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            archives_under(&p, out);
        } else if p
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("npk") || x.eq_ignore_ascii_case("mpk"))
        {
            out.push(p);
        }
    }
}

#[test]
fn map_slot_zero_versus_shader_texture_zero() {
    let root = client_root();
    assert!(
        root.join("figures").is_dir(),
        "no client at {} — a census that cannot read the tree must not report pass (REQ-025)",
        root.display()
    );
    // **`zones` is in the default scope because `figures` alone has a zero denominator.** Measured
    // 2026-08-18: over `figures` this census finds 2,302 NIFs, 652 texturing properties, and *zero*
    // with both a base map and a shader texture — so it compared nothing and reported ok. Over
    // `zones` it finds 501 with both and 33 that disagree, which is the population the shader-0 fix
    // was about. A gate whose denominator is empty is not a gate.
    let dirs = std::env::var("CAER_CENSUS_DIRS").unwrap_or_else(|_| "figures,zones".to_string());

    let mut archives = Vec::new();
    for d in dirs.split(',') {
        archives_under(&root.join(d.trim()), &mut archives);
    }
    archives.sort();
    println!("census over {}: {} archives", dirs, archives.len());

    let (mut nifs, mut props, mut multi, mut disagree) = (0usize, 0usize, 0usize, 0usize);
    let mut examples: Vec<String> = Vec::new();
    let mut chosen: Vec<(String, String)> = Vec::new();

    for arch in &archives {
        let Ok(names) = caer_assets::list_names(arch) else {
            continue;
        };
        for n in names {
            if !n.to_ascii_lowercase().ends_with(".nif") {
                continue;
            }
            let Ok(Some(m)) = caer_assets::open_first(arch, |x| x == n) else {
                continue;
            };
            let Ok(stages) = caer_assets::nif::texturing_stages(&m.data) else {
                continue;
            };
            nifs += 1;
            for (_blk, map_slots, slots) in &stages {
                props += 1;
                let map0 = slots.iter().find(|(s, _)| *s == 0).map(|(_, f)| f.as_str());
                let sh0 = slots
                    .iter()
                    .find(|(s, _)| *s == *map_slots)
                    .map(|(_, f)| f.as_str());
                let (Some(a), Some(b)) = (map0, sh0) else {
                    continue;
                };
                multi += 1;
                if !a.eq_ignore_ascii_case(b) {
                    disagree += 1;
                    chosen.push((b.to_string(), a.to_string()));
                    if examples.len() < 25 {
                        examples.push(format!(
                            "{}:{}  map0={a}  shader0={b}",
                            arch.file_name().unwrap_or_default().to_string_lossy(),
                            n
                        ));
                    }
                }
            }
        }
    }

    println!(
        "\n{nifs} NIFs, {props} texturing properties, {multi} with both a base map and a shader \
         texture, {disagree} DISAGREE"
    );
    // The denominator IS the calibration. If nothing in scope carries both a base map and a shader
    // texture, this census compared nothing — and a comparison of nothing has always come back
    // green. That is how it passed for however long the default scope was `figures`.
    assert!(
        multi > 0,
        "census scope `{dirs}` contains no NIF with both a base map and a shader texture, so this \
         gate compared nothing and its pass means nothing. Widen CAER_CENSUS_DIRS."
    );
    // And the population must actually contain disagreements, or the comparison is trivially
    // satisfied and could not tell a correct base selection from a broken one.
    assert!(
        disagree > 0,
        "census scope `{dirs}` has {multi} candidates and not one disagreement — either the client \
         tree changed or this instrument stopped being able to see the difference it exists to \
         report."
    );
    // A binding that changes to a texture the client does not ship is a regression, not a fix.
    // This is the cheap structural half of verifying the switch; in-world visual confirmation of
    // dungeon interiors needs a live session and is NOT covered here.
    let mut inventory: std::collections::BTreeSet<String> = Default::default();
    // Walk the census dirs for LOOSE textures too, recursively. A first pass listed only a few
    // fixed directories and reported 9 Agramon textures "missing" that ship in `frontiers/NIFS/`
    // — the search path was the defect, not the asset.
    fn loose_textures(dir: &Path, out: &mut std::collections::BTreeSet<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                loose_textures(&p, out);
            } else if let Some(n) = p.file_name() {
                out.insert(n.to_string_lossy().to_ascii_lowercase());
            }
        }
    }
    for d in dirs.split(',') {
        loose_textures(&root.join(d.trim()), &mut inventory);
    }
    for d in ["pregame", "pregame/textures", "items", "effects"] {
        loose_textures(&root.join(d), &mut inventory);
    }
    for a in &archives {
        if let Ok(names) = caer_assets::list_names(a) {
            inventory.extend(names.into_iter().map(|n| n.to_ascii_lowercase()));
        }
    }
    let mut missing = 0usize;
    for (name, _old) in &chosen {
        let base = name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(name)
            .to_ascii_lowercase();
        if !inventory.contains(&base) {
            missing += 1;
            println!("  MISSING on disk: {name}");
        }
    }
    println!(
        "chosen shader-0 textures resolving on disk: {}/{} ({missing} missing)",
        chosen.len() - missing,
        chosen.len()
    );
    if multi > 0 {
        println!(
            "disagreement rate among shader materials: {:.1}%",
            100.0 * disagree as f64 / multi as f64
        );
    }
    for e in &examples {
        println!("  {e}");
    }
}

/// Does every race's head mesh actually have a `<mesh>.dds` face texture?
///
/// The renderer binds a head's texture by mesh name. If that file only ships for a few races, the
/// rest render an untextured skull — which is what a playtest reports as "no face".
#[test]
fn head_texture_exists_for_every_race() {
    let root = client_root();
    let Ok(figs) = caer_assets::figures::FigureModels::load(root.join("gamedata.mpk")) else {
        println!("no fig3 tables");
        return;
    };
    let mut archives = Vec::new();
    archives_under(&root.join("figures"), &mut archives);
    archives.sort();
    let mut inv: std::collections::BTreeSet<String> = Default::default();
    for a in &archives {
        if let Ok(names) = caer_assets::list_names(a) {
            inv.extend(names.into_iter().map(|n| n.to_ascii_lowercase()));
        }
    }
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
    let (mut have, mut miss) = (Vec::new(), Vec::new());
    for (race, label) in races {
        for (g, gl) in [(1u8, "m"), (caer_assets::figures::GENDER_FEMALE, "f")] {
            let parts = figs.base_body(race, g);
            let Some(head) = parts
                .iter()
                .find(|p| p.filename.to_lowercase().contains("head"))
            else {
                continue;
            };
            let want = format!("{}.dds", head.filename).to_ascii_lowercase();
            if inv.contains(&want) {
                have.push(format!("{label} {gl}"));
            } else {
                miss.push(format!("{label} {gl} -> {want}"));
            }
        }
    }
    println!("\nhead <mesh>.dds present: {} combos", have.len());
    for h in &have {
        println!("  HAVE {h}");
    }
    println!("\nMISSING: {} combos", miss.len());
    for m in &miss {
        println!("  MISS {m}");
    }
}

/// PROBE: dump each race's head texture to PNG so a human (or Claude) can look at it.
#[test]
fn dump_head_textures() {
    let root = client_root();
    let Some(out) = std::env::var_os("CAER_PROBE_OUT") else {
        return;
    };
    let Ok(figs) = caer_assets::figures::FigureModels::load(root.join("gamedata.mpk")) else {
        return;
    };
    let mut archives = Vec::new();
    archives_under(&root.join("figures"), &mut archives);
    archives.sort();
    for (race, label) in [(8u8, "Kobold"), (1, "Briton"), (6, "Troll"), (9, "Celt")] {
        for (g, gl) in [(1u8, "m"), (caer_assets::figures::GENDER_FEMALE, "f")] {
            let parts = figs.base_body(race, g);
            let Some(head) = parts
                .iter()
                .find(|p| p.filename.to_lowercase().contains("head"))
            else {
                continue;
            };
            let want = format!("{}.dds", head.filename);
            for a in &archives {
                if let Ok(Some(m)) = caer_assets::open_first(a, |n| n.eq_ignore_ascii_case(&want)) {
                    let p = std::path::Path::new(&out).join(format!("head_{label}_{gl}.dds"));
                    let _ = std::fs::write(&p, &m.data);
                    println!(
                        "wrote {label} {gl}: {want} ({} bytes) from {}",
                        m.data.len(),
                        a.file_name().unwrap_or_default().to_string_lossy()
                    );
                    break;
                }
            }
        }
    }
}

/// Where does each head mesh sample its texture sheet?
///
/// The sheet is one face plus small detail patches (ear, teeth, eyeball) in the lower region. If a
/// known-good head and a known-bad head sample different areas, the defect is UV mapping, not the
/// asset or the binding.
#[test]
fn head_uv_bounds_per_race() {
    let root = client_root();
    let Ok(figs) = caer_assets::figures::FigureModels::load(root.join("gamedata.mpk")) else {
        return;
    };
    println!(
        "{:<14}{:>8}{:>26}{:>26}",
        "race/gender", "verts", "u range", "v range"
    );
    for (race, label) in [
        (1u8, "Briton GOOD"),
        (6, "Troll GOOD"),
        (8, "Kobold BAD"),
        (9, "Celt BAD"),
        (5, "Norseman BAD"),
        (7, "Dwarf BAD"),
        (16, "HalfOgre BAD"),
    ] {
        for (g, gl) in [(1u8, "m"), (caer_assets::figures::GENDER_FEMALE, "f")] {
            let parts = figs.base_body(race, g);
            let Some(head) = parts
                .iter()
                .find(|p| p.filename.to_lowercase().contains("head"))
            else {
                continue;
            };
            let Ok(Some(hb)) =
                caer_assets::open_member(root.join(head.archive_path()), &head.nif_member())
            else {
                continue;
            };
            let Ok(m) = caer_assets::nif::read_model(&hb) else {
                continue;
            };
            for p in &m.parts {
                if p.uvs.is_empty() || p.positions.len() < 100 {
                    continue;
                }
                let (mut u0, mut u1, mut v0, mut v1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
                for uv in &p.uvs {
                    if uv[0].is_finite() && uv[1].is_finite() {
                        u0 = u0.min(uv[0]);
                        u1 = u1.max(uv[0]);
                        v0 = v0.min(uv[1]);
                        v1 = v1.max(uv[1]);
                    }
                }
                println!(
                    "{:<14}{:>8}{:>13.3}..{:<12.3}{:>12.3}..{:<12.3}  {}",
                    format!("{label} {gl}"),
                    p.positions.len(),
                    u0,
                    u1,
                    v0,
                    v1,
                    head.nif_member()
                );
            }
        }
    }
}

/// PROBE: decode head textures through OUR decoder and dump RGBA, to compare against the file.
///
/// The binding is correct and the file contains a face, yet the head renders grey with dark
/// sockets. That leaves our own decode as the suspect.
#[test]
fn decode_head_textures_through_our_path() {
    let root = client_root();
    let Some(out) = std::env::var_os("CAER_PROBE_OUT") else {
        return;
    };
    let mut archives = Vec::new();
    archives_under(&root.join("figures"), &mut archives);
    archives.sort();
    for want in ["kob_m_head01.dds", "bri_m_head01.dds", "tro_m_head01.dds"] {
        for a in &archives {
            let Ok(Some(m)) = caer_assets::open_first(a, |n| n.eq_ignore_ascii_case(want)) else {
                continue;
            };
            match caer_assets::dds::read_model_dds(&m.data) {
                Ok(t) => {
                    println!(
                        "{want}: {}x{} format {:?} mips {}",
                        t.width,
                        t.height,
                        t.format,
                        t.mips.len()
                    );
                    match t.rgba8_mip0() {
                        Some((w, h, px)) => {
                            let p = std::path::Path::new(&out).join(format!("ours_{want}.raw"));
                            let _ = std::fs::write(&p, &px);
                            println!("   rgba8 {w}x{h} {} bytes -> {}", px.len(), p.display());
                        }
                        None => println!("   rgba8_mip0 returned None"),
                    }
                }
                Err(e) => println!("{want}: OUR DECODE FAILED: {e}"),
            }
            break;
        }
    }
}

/// Alpha coverage of every race's head texture.
///
/// The claim "one face per file, 100% opaque" was measured on bri_m, bri_f, hig_m and tro_m, which
/// are exactly the races whose faces render correctly. If the others carry transparency, an alpha
/// cutout would erase most of the face and leave only the darkest patches.
#[test]
fn head_texture_alpha_coverage() {
    let root = client_root();
    let Ok(figs) = caer_assets::figures::FigureModels::load(root.join("gamedata.mpk")) else {
        return;
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
    println!("race/gender      format    opaque %   min alpha  file");
    for (race, label) in races {
        for (g, gl) in [(1u8, "m"), (caer_assets::figures::GENDER_FEMALE, "f")] {
            let parts = figs.base_body(race, g);
            let Some(head) = parts
                .iter()
                .find(|p| p.filename.to_lowercase().contains("head"))
            else {
                continue;
            };
            let want = format!("{}.dds", head.filename);
            for a in &archives {
                let Ok(Some(m)) = caer_assets::open_first(a, |n| n.eq_ignore_ascii_case(&want))
                else {
                    continue;
                };
                let Ok(t) = caer_assets::dds::read_model_dds(&m.data) else {
                    break;
                };
                let Some((_, _, px)) = t.rgba8_mip0() else {
                    break;
                };
                let total = px.len() / 4;
                let opaque = px.chunks_exact(4).filter(|c| c[3] >= 250).count();
                let min = px.chunks_exact(4).map(|c| c[3]).min().unwrap_or(255);
                println!(
                    "{:<16}{:>10}{:>11.1}%{:>12}  {want}",
                    format!("{label} {gl}"),
                    format!("{:?}", t.format),
                    100.0 * opaque as f64 / total as f64,
                    min
                );
                break;
            }
        }
    }
}

/// Hair mesh extents against the head it sits on.
///
/// A "hair" that is nearly the size of the head is a full skull covering. Bound to a pale texture
/// it would hide the face and leave only gaps showing, which is what the broken races look like.
#[test]
fn hair_mesh_versus_head_extent() {
    let root = client_root();
    let Ok(figs) = caer_assets::figures::FigureModels::load(root.join("gamedata.mpk")) else {
        return;
    };
    let ext = |b: &[u8]| -> Option<[f32; 3]> {
        let m = caer_assets::nif::read_model(b).ok()?;
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for p in &m.parts {
            for v in &p.positions {
                if !v.iter().all(|c| c.is_finite()) {
                    continue;
                }
                for a in 0..3 {
                    lo[a] = lo[a].min(v[a]);
                    hi[a] = hi[a].max(v[a]);
                }
            }
        }
        (lo[0] <= hi[0]).then(|| [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]])
    };
    println!(
        "{:<16}{:>22}{:>22}{:>10}",
        "race/gender", "head extent", "hair extent", "hair/head"
    );
    for (race, label) in [
        (1u8, "Briton GOOD"),
        (6, "Troll GOOD"),
        (8, "Kobold BAD"),
        (5, "Norseman BAD"),
        (7, "Dwarf BAD"),
        (16, "HalfOgre BAD"),
        (11, "Elf BAD"),
    ] {
        for (g, gl) in [(1u8, "m"), (caer_assets::figures::GENDER_FEMALE, "f")] {
            let parts = figs.base_body(race, g);
            let get = |kind: &str| -> Option<[f32; 3]> {
                let p = parts
                    .iter()
                    .find(|p| p.filename.to_lowercase().contains(kind))?;
                let b = caer_assets::open_member(root.join(p.archive_path()), &p.nif_member())
                    .ok()??;
                ext(&b)
            };
            let (Some(h), Some(r)) = (get("head"), get("hair")) else {
                continue;
            };
            let vol_h = h[0] * h[1] * h[2];
            let vol_r = r[0] * r[1] * r[2];
            println!(
                "{:<16}{:>7.1}x{:>5.1}x{:>5.1}{:>8.1}x{:>5.1}x{:>5.1}{:>10.2}",
                format!("{label} {gl}"),
                h[0],
                h[1],
                h[2],
                r[0],
                r[1],
                r[2],
                vol_r / vol_h.max(0.001)
            );
        }
    }
}

/// Every copy of a head texture across the client, with its format.
///
/// The GPU receives Bc2 for Kobold and Bc1 for Briton, while reading the same names from
/// `figures/` gives Bc3 for both. Either duplicate copies differ, or format detection differs
/// between the two load paths.
#[test]
fn head_texture_copies_and_formats() {
    let root = client_root();
    let mut archives = Vec::new();
    for d in ["figures", "items", "effects"] {
        archives_under(&root.join(d), &mut archives);
    }
    archives.sort();
    for want in [
        "kob_m_head01.dds",
        "bri_m_head01.dds",
        "hig_m_head01.dds",
        "tro_m_head01.dds",
    ] {
        println!("\n=== {want} ===");
        let mut n = 0;
        for a in &archives {
            let Ok(Some(m)) = caer_assets::open_first(a, |x| x.eq_ignore_ascii_case(want)) else {
                continue;
            };
            n += 1;
            let f = caer_assets::dds::read_model_dds(&m.data)
                .map(|t| {
                    format!(
                        "{}x{} {:?} mips {}",
                        t.width,
                        t.height,
                        t.format,
                        t.mips.len()
                    )
                })
                .unwrap_or_else(|e| format!("DECODE FAILED: {e}"));
            // The raw fourCC as it sits in the file header.
            let cc = if m.data.len() > 88 {
                String::from_utf8_lossy(&m.data[84..88]).to_string()
            } else {
                "?".into()
            };
            println!(
                "  {:<18} {:>8} bytes  fourCC {cc:<6} -> {f}",
                a.file_name().unwrap_or_default().to_string_lossy(),
                m.data.len()
            );
        }
        if n == 0 {
            println!("  no copies found");
        }
    }
}

/// PROBE: dump the creature-skin copy of a head texture next to the Mskins copy.
#[test]
fn dump_rival_head_copies() {
    let root = client_root();
    let Some(out) = std::env::var_os("CAER_PROBE_OUT") else {
        return;
    };
    for (arch, tag) in [
        ("figures/skins/skin149.mpk", "kob_skin149"),
        ("figures/Mskins/mskin006.mpk", "kob_mskin006"),
        ("figures/skins/skin142.mpk", "bri_skin142"),
    ] {
        let want = if tag.starts_with("bri") {
            "bri_m_head01.dds"
        } else {
            "kob_m_head01.dds"
        };
        let Ok(Some(m)) =
            caer_assets::open_first(root.join(arch), |n| n.eq_ignore_ascii_case(want))
        else {
            println!("{tag}: not found in {arch}");
            continue;
        };
        let Ok(t) = caer_assets::dds::read_model_dds(&m.data) else {
            continue;
        };
        let Some((w, h, px)) = t.rgba8_mip0() else {
            continue;
        };
        let p = std::path::Path::new(&out).join(format!("rival_{tag}.raw"));
        let _ = std::fs::write(&p, &px);
        println!("{tag}: {w}x{h} {:?} -> {}", t.format, p.display());
    }
}

/// PROBE: dump the texture the RENDERER picks (look-table face skin), not the mesh-named one.
#[test]
fn dump_looktable_face_textures() {
    let root = client_root();
    let Some(out) = std::env::var_os("CAER_PROBE_OUT") else {
        return;
    };
    let mut archives = Vec::new();
    archives_under(&root.join("figures"), &mut archives);
    archives.sort();
    for want in [
        "cel_m_head01.dds",
        "bri_m_head01.dds",
        "hig_m_head01.dds",
        "tro_m_head01.dds",
    ] {
        for a in &archives {
            let Ok(Some(m)) = caer_assets::open_first(a, |n| n.eq_ignore_ascii_case(want)) else {
                continue;
            };
            let Ok(t) = caer_assets::dds::read_model_dds(&m.data) else {
                break;
            };
            let Some((w, h, px)) = t.rgba8_mip0() else {
                break;
            };
            let p = std::path::Path::new(&out).join(format!("look_{want}.raw"));
            let _ = std::fs::write(&p, &px);
            println!(
                "{want}: {w}x{h} {:?} from {}",
                t.format,
                a.file_name().unwrap_or_default().to_string_lossy()
            );
            break;
        }
    }
}

/// Alpha of the face textures the RENDERER actually loads, and how much of each survives the
/// skinned shader's 0.4 alpha cutout.
///
/// The earlier alpha pass measured `<mesh>.dds`, but faces come from the look tables and are
/// different files (Celt loads `cel_m_head01.dds`, not `bri_m_head01.dds`). Clothing is forced
/// opaque before upload; the head is not.
#[test]
fn looktable_face_alpha_survives_cutout() {
    let root = client_root();
    let mut archives = Vec::new();
    archives_under(&root.join("figures"), &mut archives);
    archives.sort();
    println!("texture                   format  alpha>=0.4 %    mean alpha  archive");
    for want in [
        "bri_m_head01.dds",
        "tro_m_head01.dds",
        "cel_m_head01.dds",
        "hig_m_head01.dds",
        "kob_m_head01.dds",
        "hal_m_head01.dds",
    ] {
        for a in &archives {
            let Ok(Some(m)) = caer_assets::open_first(a, |n| n.eq_ignore_ascii_case(want)) else {
                continue;
            };
            let Ok(t) = caer_assets::dds::read_model_dds(&m.data) else {
                break;
            };
            let Some((_, _, px)) = t.rgba8_mip0() else {
                break;
            };
            let n = px.len() / 4;
            let keep = px
                .chunks_exact(4)
                .filter(|c| c[3] as f32 / 255.0 >= 0.4)
                .count();
            let mean: f64 = px.chunks_exact(4).map(|c| c[3] as f64).sum::<f64>() / n as f64;
            println!(
                "{want:<22}{:>10}{:>13.1}%{:>14.1}  {}",
                format!("{:?}", t.format),
                100.0 * keep as f64 / n as f64,
                mean,
                a.file_name().unwrap_or_default().to_string_lossy()
            );
            break;
        }
    }
}

/// PROBE: dump a head mesh's UV triangles so they can be projected onto each loaded face texture.
///
/// "UVs span 0..1" was true and useless: it measures the bounding box, not where the face polygons
/// land. This writes the actual islands.
#[test]
fn dump_head_uv_triangles() {
    let root = client_root();
    let Some(out) = std::env::var_os("CAER_PROBE_OUT") else {
        return;
    };
    let mut archives = Vec::new();
    archives_under(&root.join("figures"), &mut archives);
    archives.sort();
    for want in ["bri_m_head01.nif", "kob_m_head01.nif"] {
        for a in &archives {
            let Ok(Some(m)) = caer_assets::open_first(a, |n| n.eq_ignore_ascii_case(want)) else {
                continue;
            };
            let Ok(model) = caer_assets::nif::read_model(&m.data) else {
                break;
            };
            let mut s = String::new();
            for p in &model.parts {
                if p.uvs.is_empty() {
                    continue;
                }
                s.push_str(&format!(
                    "PART {} {} {}\n",
                    p.name,
                    p.uvs.len(),
                    p.indices.len()
                ));
                for uv in &p.uvs {
                    s.push_str(&format!("UV {} {}\n", uv[0], uv[1]));
                }
                for t in p.indices.chunks(3) {
                    if t.len() == 3 {
                        s.push_str(&format!("TRI {} {} {}\n", t[0], t[1], t[2]));
                    }
                }
            }
            let p = std::path::Path::new(&out).join(format!("uv_{want}.txt"));
            let _ = std::fs::write(&p, s);
            println!("{want}: {} parts -> {}", model.parts.len(), p.display());
            break;
        }
    }
}

/// GATE: our DXT3/DXT5 alpha decode must match an independent decoder.
///
/// `rgba8_mip0` used to discard the alpha block and let BC1's implicit 255 stand, so every alpha
/// reading was "100% opaque" regardless of the file. Face sheets average ~0.2 and `skinned.wgsl`
/// discards under 0.4, which erased every face while three separate alpha checks reported healthy.
#[test]
fn dxt_alpha_is_decoded_not_assumed_opaque() {
    let root = client_root();
    if !root.join("figures").is_dir() {
        return;
    }
    let mut archives = Vec::new();
    archives_under(&root.join("figures"), &mut archives);
    archives.sort();
    let mut checked = 0;
    for want in [
        "hig_m_head01.dds",
        "cel_m_head01.dds",
        "kob_m_head01.dds",
        "tro_f_head01.dds",
    ] {
        for a in &archives {
            let Ok(Some(m)) = caer_assets::open_first(a, |n| n.eq_ignore_ascii_case(want)) else {
                continue;
            };
            let Ok(t) = caer_assets::dds::read_model_dds(&m.data) else {
                break;
            };
            if t.format != caer_assets::dds::DdsFormat::Bc3 {
                break;
            }
            let Some((_, _, px)) = t.rgba8_mip0() else {
                break;
            };
            let n = px.len() / 4;
            let mean: f64 = px.chunks_exact(4).map(|c| c[3] as f64).sum::<f64>() / n as f64;
            let opaque = px.chunks_exact(4).filter(|c| c[3] == 255).count();
            println!(
                "{want}: mean alpha {mean:.1}, {:.1}% fully opaque",
                100.0 * opaque as f64 / n as f64
            );
            // These sheets are measurably NOT opaque (PIL puts their mean near 0.2 * 255 = ~51).
            assert!(
                mean < 200.0,
                "{want} decodes as mean alpha {mean:.1}; the alpha block is being ignored again"
            );
            checked += 1;
            break;
        }
    }
    assert!(
        checked >= 2,
        "expected at least two Bc3 head sheets, checked {checked}"
    );
}

/// Does every race/gender have its OWN hair row, or does it borrow another's?
///
/// Inconnu male renders `inc_f_hair01_white.dds`, a FEMALE sheet. Either the male row is absent
/// and something falls back wrongly, or the table genuinely points there.
#[test]
fn hair_rows_exist_per_race_and_gender() {
    let root = client_root();
    let Ok(look) = caer_assets::fig3_look::Fig3Look::load(root.join("gamedata.mpk")) else {
        println!("fig3_look did not load");
        return;
    };
    let races = [
        "Briton",
        "Avalonian",
        "Highlander",
        "Saracen",
        "Norseman",
        "Troll",
        "Dwarf",
        "Kobold",
        "Celt",
        "Firbolg",
        "Elf",
        "Lurikeen",
        "Inconnu",
        "Valkyn",
        "Sylvan",
        "HalfOgre",
        "Frostalf",
        "Shar",
    ];
    let mut missing = Vec::new();
    for r in races {
        for (g, gl) in [(1u8, "m"), (2, "f")] {
            let hair = look.hair_skin_id(r, g, 1);
            let face = look.face_skin_id(r, g, 1);
            if hair.is_none() || face.is_none() {
                missing.push(format!("{r} {gl}: hair={hair:?} face={face:?}"));
            }
        }
    }
    println!("rows missing from the fig3 look tables: {}", missing.len());
    for m in &missing {
        println!("  {m}");
    }
}

/// PROBE: race names as spelled in the fig3 look tables, vs the names we look up with.
#[test]
fn fig3_table_race_names() {
    let root = client_root();
    let Ok(members) = caer_assets::open(root.join("gamedata.mpk")) else {
        return;
    };
    for want in ["fig3haircolormap.csv", "fig3facemap.csv"] {
        let Some(m) = members.iter().find(|m| m.name.eq_ignore_ascii_case(want)) else {
            println!("{want}: not found");
            continue;
        };
        let text = String::from_utf8_lossy(&m.data);
        let mut names: std::collections::BTreeSet<String> = Default::default();
        for line in text.lines().skip(1) {
            if let Some(first) = line.split(',').next() {
                let f = first.trim();
                if !f.is_empty() && f.chars().any(|c| c.is_alphabetic()) {
                    names.insert(f.to_string());
                }
            }
        }
        println!("\n{want}: {} distinct first-column values", names.len());
        println!("  {:?}", names.iter().take(30).collect::<Vec<_>>());
    }
}

/// PROBE: write the fig3 look tables to disk for direct inspection.
#[test]
fn dump_fig3_tables() {
    let root = client_root();
    let Some(out) = std::env::var_os("CAER_PROBE_OUT") else {
        return;
    };
    let Ok(members) = caer_assets::open(root.join("gamedata.mpk")) else {
        return;
    };
    for want in [
        "fig3haircolormap.csv",
        "fig3facemap.csv",
        "fig3skincolormap.csv",
    ] {
        match members.iter().find(|m| m.name.eq_ignore_ascii_case(want)) {
            Some(m) => {
                let p = std::path::Path::new(&out).join(want);
                let _ = std::fs::write(&p, &m.data);
                println!("{want}: {} bytes -> {}", m.data.len(), p.display());
            }
            None => println!("{want}: NOT IN gamedata.mpk"),
        }
    }
}

/// PROBE: hair/face skin ids per race+gender. Identical ids across genders means the gender key is
/// not discriminating, which would explain a male wearing a female sheet.
#[test]
fn hair_face_ids_by_gender() {
    let root = client_root();
    let Ok(look) = caer_assets::fig3_look::Fig3Look::load(root.join("gamedata.mpk")) else {
        return;
    };
    println!(
        "{:<14}{:>10}{:>10}{:>10}{:>10}",
        "race", "hair m", "hair f", "face m", "face f"
    );
    for r in [
        "Briton",
        "Norseman",
        "Inconnu",
        "Kobold",
        "HalfOgre",
        "Highlander",
    ] {
        let hm = look.hair_skin_id(r, 1, 1);
        let hf = look.hair_skin_id(r, 2, 1);
        let fm = look.face_skin_id(r, 1, 1);
        let ff = look.face_skin_id(r, 2, 1);
        let same = if hm == hf {
            "  <- hair ids identical"
        } else {
            ""
        };
        println!(
            "{r:<14}{:>10}{:>10}{:>10}{:>10}{same}",
            hm.map_or("-".into(), |v| v.to_string()),
            hf.map_or("-".into(), |v| v.to_string()),
            fm.map_or("-".into(), |v| v.to_string()),
            ff.map_or("-".into(), |v| v.to_string()),
        );
    }
}

/// PROBE: which hair MESH does fig3 assign each race? Half Ogre renders `hig_m_hair03`.
#[test]
fn hair_mesh_assignment_by_race() {
    let root = client_root();
    let Ok(figs) = caer_assets::figures::FigureModels::load(root.join("gamedata.mpk")) else {
        return;
    };
    for (race, label) in [
        (16u8, "HalfOgre"),
        (3, "Highlander"),
        (1, "Briton"),
        (13, "Inconnu"),
    ] {
        for (g, gl) in [(1u8, "m"), (caer_assets::figures::GENDER_FEMALE, "f")] {
            let parts = figs.base_body(race, g);
            let hair: Vec<String> = parts
                .iter()
                .filter(|p| p.filename.to_lowercase().contains("hair"))
                .map(|p| format!("{} ({})", p.nif_member(), p.archive_path()))
                .collect();
            println!("{label:<12}{gl}  {hair:?}");
        }
    }
}

/// PROBE: alpha of the Albion char-screen tower textures.
///
/// The tower's front reads as see-through and its interior is visible. `mesh.wgsl` discards low
/// alpha exactly as the skinned shader does, which is what erased every face.
#[test]
fn albion_tower_texture_alpha() {
    let root = client_root();
    for name in [
        "b_rndtwr_exterior.dds",
        "b_rndtwr_interior.dds",
        "a_brick01.dds",
        "a_woodplanks01.dds",
    ] {
        let mut found = false;
        for dir in ["pregame", "pregame/textures", "items", "effects"] {
            let p = root.join(dir).join(name);
            let Ok(bytes) = std::fs::read(&p) else {
                continue;
            };
            let Ok(t) = caer_assets::dds::read_model_dds(&bytes) else {
                continue;
            };
            let Some((_, _, px)) = t.rgba8_mip0() else {
                continue;
            };
            let n = px.len() / 4;
            let keep = px.chunks_exact(4).filter(|c| c[3] >= 102).count();
            let mean: f64 = px.chunks_exact(4).map(|c| c[3] as f64).sum::<f64>() / n as f64;
            println!(
                "{name:<26} {:?} mean alpha {mean:.1}  survives 0.4 cutout {:.1}%",
                t.format,
                100.0 * keep as f64 / n as f64
            );
            found = true;
            break;
        }
        if !found {
            println!("{name:<26} not found loose");
        }
    }
}

/// PROBE: does the hair colour map's `style #` correspond to the mesh's `hairNN` suffix?
///
/// Half Ogre male's mesh is `hig_m_hair03` (style 3) but the parse is first-wins and always takes
/// style#1. If style#3's id resolves to a hair03 sheet, the style index is the bug.
#[test]
fn hair_style_row_versus_mesh_suffix() {
    let root = client_root();
    let Ok(res) = caer_assets::monsters::MonsterModels::load(root.join("gamedata.mpk")) else {
        println!("resolver did not load");
        return;
    };
    for (id, label) in [
        (3867u16, "HalfOgre m style#1"),
        (3871, "HalfOgre m style#3"),
        (3456, "Kobold m style#1"),
        (3460, "Kobold m style#3"),
        (3804, "Briton m style#1"),
        (3808, "Briton m style#2"),
    ] {
        match res.skin(id) {
            Some(s) => println!(
                "{label:<22} id {id} -> {} (skin{:03}.mpk)",
                s.dds, s.archive
            ),
            None => println!("{label:<22} id {id} -> unresolved"),
        }
    }
}

/// PROBE: every hair style row per race, resolved to its DDS, next to the mesh fig3 assigns.
///
/// The style index selects a hairstyle; mesh and texture must come from the SAME style. If a row
/// exists whose sheet matches the mesh's `hairNN` stem, pairing to it is a fix using only client
/// data.
#[test]
fn hair_styles_versus_assigned_mesh() {
    let root = client_root();
    let (Ok(res), Ok(figs), Ok(look)) = (
        caer_assets::monsters::MonsterModels::load(root.join("gamedata.mpk")),
        caer_assets::figures::FigureModels::load(root.join("gamedata.mpk")),
        caer_assets::fig3_look::Fig3Look::load(root.join("gamedata.mpk")),
    ) else {
        return;
    };
    for (race, label) in [
        (8u8, "Kobold"),
        (16, "HalfOgre"),
        (1, "Briton"),
        (5, "Norseman"),
        (13, "Inconnu"),
    ] {
        for (g, gl) in [(1u8, "m"), (caer_assets::figures::GENDER_FEMALE, "f")] {
            let mesh = figs
                .base_body(race, g)
                .into_iter()
                .find(|p| p.filename.to_lowercase().contains("hair"))
                .map(|p| p.filename.to_lowercase());
            let Some(mesh) = mesh else { continue };
            let mut rows = Vec::new();
            for style in 1..=10u8 {
                if let Some(id) = look
                    .hair_styles(label, g, 1)
                    .into_iter()
                    .find(|(st, _)| *st == style)
                    .map(|(_, i)| i)
                {
                    if let Some(s) = res.skin(id) {
                        rows.push(format!("s{style}:{}", s.dds));
                    }
                }
            }
            println!("{label:<10}{gl}  mesh {mesh:<18} styles {rows:?}");
        }
    }
}

/// PROBE: does a hair MESH exist named after each style's SHEET?
///
/// A hairstyle selects both. If sheet `kob_m_hair02_white.dds` has a `kob_m_hair02.nif`, then
/// deriving the mesh from the sheet pairs them using only client data.
#[test]
fn hair_mesh_exists_for_each_style_sheet() {
    let root = client_root();
    let mut archives = Vec::new();
    archives_under(&root.join("figures"), &mut archives);
    archives.sort();
    let mut have: std::collections::BTreeSet<String> = Default::default();
    for a in &archives {
        if let Ok(names) = caer_assets::list_names(a) {
            have.extend(names.into_iter().map(|n| n.to_ascii_lowercase()));
        }
    }
    for sheet in [
        "kob_m_hair02_white.dds",
        "hal_m_hair01_white.dds",
        "nor_m_hair02_white.dds",
        "inc_f_hair01_white.dds",
        "bri_m_hair01_blonde.dds",
        "hig_m_hair1_blonde.dds",
    ] {
        // sheet stem -> mesh: strip the colour suffix and swap the extension.
        let stem = sheet.rsplit_once('.').map_or(sheet, |(s, _)| s);
        let mesh_stem = stem.rsplit_once('_').map_or(stem, |(s, _)| s);
        let mesh = format!("{mesh_stem}.nif");
        println!(
            "{sheet:<28} -> {mesh:<24} {}",
            if have.contains(&mesh) {
                "EXISTS"
            } else {
                "MISSING"
            }
        );
    }
}
