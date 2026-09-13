//! Player-avatar harness: what every legal body resolves, binds, poses and looks like.
//!
//! One file for one concern. This absorbed `avatar_matrix`, `avatar_contact_sheet` and
//! `appearance_axes`, which had grown as three separate harnesses chasing the same subject and had
//! three copies of the client-root rule and the legal-race enumeration between them.
//!
//! # What lives here
//!
//! * the per-race resolution matrix (model, path, binds, rig, scale, bind-error distribution)
//! * the contact sheet — every body in one image, because a wrong asset has to be SEEN
//! * the appearance axes the matrix does not cover: worn armour, creation variants, tints
//! * the checks that separate "bound a texture" from "drew a visible part"
//!
//! # Why the sheet exists at all
//!
//! Two wrong material binders shipped in a row behind `mskin_bound = 5, white = 0` — a Highlander
//! in an undead skeleton texture, one in generic cloth, and the correct tartan all score the same.
//! A count answers *did anything bind*; only the image answers *did the right thing bind*.
//!
//! **Fails** when the retail tree is absent (REQ-025). Set `CAER_CLIENT`.

mod common;

use std::collections::BTreeSet;
use std::path::PathBuf;

use caer_render::camera::Camera;
use caer_render::entities::EntityModels;
use caer_render::gpu::{FramePass, Gpu};
use common::{coverage, legal_combinations, root};
use glam::Vec3;

/// Per-body cell in the contact sheet. Tall and narrow: a standing figure.
const CELL_W: u32 = 200;
const CELL_H: u32 = 300;
const COLS: u32 = 8;

/// One assembled body, as the screens would show it.
struct Row {
    race: u8,
    label: &'static str,
    /// Wire gender (0 male, 1 female) — the value the create payload carries.
    wire_gender: u8,
    model: Option<u16>,
    path: &'static str,
    fig3_parts: usize,
    textured: usize,
    mskin_bound: usize,
    eq_bound: usize,
    rigged: bool,
    clip_duration: f32,
    scale: f32,
    height: Option<f32>,
    parts: String,
    bound: Vec<(String, String)>,
    bind_errors: Vec<(String, f32)>,
}

impl Row {
    fn gender_name(&self) -> &'static str {
        if self.wire_gender == 0 {
            "male"
        } else {
            "female"
        }
    }

    /// Parts that bound NO texture — the ones a player sees as white.
    ///
    /// Counted from the per-part bindings, not by subtracting counters. The counters overlap
    /// (a part can be both `textured` and `mskin_bound`), so the arithmetic version saturated to
    /// zero and reported a Highlander with a white kilt and a blank face as fully bound.
    fn unbound_parts(&self) -> usize {
        if self.bound.is_empty() {
            return self.fig3_parts;
        }
        self.bound.iter().filter(|(_, t)| t == "<none>").count()
    }

    /// The parts that are still bare, by mesh name.
    fn bare(&self) -> Vec<&str> {
        self.bound
            .iter()
            .filter(|(_, t)| t == "<none>")
            .map(|(n, _)| n.as_str())
            .collect()
    }
}

fn assemble(
    models: &mut EntityModels,
    gpu: &mut Gpu,
    race: u8,
    label: &'static str,
    wire: u8,
) -> Row {
    let fig3_gender = caer_protocol::overview::fig3_gender_from_db(wire);
    let model = models.ensure_avatar(
        gpu,
        race,
        fig3_gender,
        caer_protocol::customization::Customization::default(),
        None,
    );
    let info = models.last_avatar_stand();
    let (path, fig3_parts, textured, mskin_bound, eq_bound, parts, bound, bind_errors) = (
        info.path,
        info.fig3_parts,
        info.textured,
        info.mskin_bound,
        info.eq_bound,
        info.part_names.clone(),
        info.bound_textures.clone(),
        info.bind_errors.clone(),
    );
    let rig = model.and_then(|m| models.skinned_rig(m));
    Row {
        race,
        label,
        wire_gender: wire,
        model,
        path,
        fig3_parts,
        textured,
        mskin_bound,
        eq_bound,
        rigged: rig.is_some(),
        clip_duration: rig.map_or(0.0, |r| r.clip.duration),
        scale: models.race_display_scale(race, fig3_gender),
        height: model.and_then(|m| models.model_height(m)),
        parts,
        bound,
        bind_errors,
    }
}

#[test]
fn every_legal_avatar_reports_what_it_bound() {
    let root = root();
    // SAFETY: single-threaded test entry; `EntityModels::load` reads the client root from env.
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(320, 240, 60_000.0)).expect("gpu init");
    let mut models = EntityModels::load().expect("entity models from the retail tree");

    let combos = legal_combinations();
    assert!(
        combos.len() >= 34,
        "the three realms offer more bodies than {} — the enumeration is wrong, not the client",
        combos.len()
    );

    let rows: Vec<Row> = combos
        .into_iter()
        .map(|(race, label, wire)| assemble(&mut models, &mut gpu, race, label, wire))
        .collect();

    println!(
        "\n{:<14} {:>3} {:<7} {:>6} {:<7} {:>5} {:>4} {:>5} {:>3} {:>6} {:>6} {:>7}  {}",
        "race",
        "id",
        "gender",
        "model",
        "path",
        "parts",
        "tex",
        "mskin",
        "eq",
        "rig",
        "scale",
        "height",
        "white"
    );
    for r in &rows {
        println!(
            "{:<14} {:>3} {:<7} {:>6} {:<7} {:>5} {:>4} {:>5} {:>3} {:>6} {:>6.2} {:>7}  {}",
            r.label,
            r.race,
            r.gender_name(),
            r.model.map_or("-".into(), |m| format!("{m:#06x}")),
            r.path,
            r.fig3_parts,
            r.textured,
            r.mskin_bound,
            r.eq_bound,
            if r.rigged {
                format!("{:.1}s", r.clip_duration)
            } else {
                "none".into()
            },
            r.scale,
            r.height.map_or("-".into(), |h| format!("{h:.0}")),
            r.unbound_parts()
        );
    }

    // WHICH texture bound, not just how many. A count cannot tell a Highlander's tartan from
    // generic cloth, and two wrong binders shipped behind exactly that number.
    // WHICH file the head and hair bind — the count above says only that something did.
    println!("\nhead / hair texture per race (the check a count cannot make):");
    for r in &rows {
        let pick = |slot: &str| -> String {
            r.bound
                .iter()
                .find(|(part, _)| part.to_ascii_lowercase().contains(slot))
                .map(|(_, tex)| tex.clone())
                .unwrap_or_else(|| "-".into())
        };
        let (head, hair) = (pick("head"), pick("hair"));
        // A head wearing a monster reskin, or nothing, is the "no face" report.
        let flag = if head == "-" || head == "<none>" {
            "  <- NO HEAD TEXTURE"
        } else if ["mummy", "shade", "stone"]
            .iter()
            .any(|m| head.to_ascii_lowercase().contains(m))
        {
            "  <- MONSTER RESKIN"
        } else {
            ""
        };
        println!(
            "  {:<12} {:<7} head={:<28} hair={}{flag}",
            r.label,
            r.gender_name(),
            head,
            hair
        );
    }

    println!("\nbound starting-set textures per race (the check a count cannot make):");
    for r in &rows {
        if r.bound.is_empty() {
            continue;
        }
        let bare = r.bare();
        println!(
            "  {:<12} {:<7} {:>2} bare{}",
            r.label,
            r.gender_name(),
            bare.len(),
            if bare.is_empty() {
                String::new()
            } else {
                format!("  -> {}", bare.join(", "))
            }
        );
    }

    // Bind-error distribution for anything that lost its rig. A uniformly huge set means the wrong
    // skeleton; a single outlier means one bad part variant. The gate alone cannot tell them apart.
    println!("\nbind errors for bodies that lost the skinned path (gate is 5.0):");
    for r in &rows {
        if r.rigged || r.bind_errors.is_empty() {
            continue;
        }
        let mut es: Vec<String> = r
            .bind_errors
            .iter()
            .map(|(n, e)| format!("{n}={e:.1}"))
            .collect();
        es.sort();
        println!("  {} {}: {}", r.label, r.gender_name(), es.join("  "));
    }

    // Detail lines only where something is actually wrong, so the report stays readable.
    println!("\nparts detail for rows that are not fully bound:");
    let mut clean = 0usize;
    for r in &rows {
        if r.unbound_parts() == 0 && r.rigged && r.model.is_some() && !r.parts.contains("miss=") {
            clean += 1;
            continue;
        }
        println!("  {} {}: {}", r.label, r.gender_name(), r.parts);
    }
    println!(
        "\n{clean}/{} combinations assembled fully textured and rigged.",
        rows.len()
    );

    // Summary by failure mode — the shape of the family, not a per-race verdict.
    let no_body: Vec<_> = rows.iter().filter(|r| r.model.is_none()).collect();
    let living: Vec<_> = rows.iter().filter(|r| r.path == "living").collect();
    let unrigged: Vec<_> = rows
        .iter()
        .filter(|r| r.model.is_some() && !r.rigged)
        .collect();
    let white: Vec<_> = rows.iter().filter(|r| r.unbound_parts() > 0).collect();
    println!(
        "\nno body at all: {}\nliving-model path: {}\nassembled but unrigged (bind pose): {}\nany white part: {}",
        summarise(&no_body),
        summarise(&living),
        summarise(&unrigged),
        summarise(&white)
    );

    // Invariants defensible today. Anything beyond these is reported, not asserted, until the
    // owning contract is established — a test that asserts a number nobody has derived is a
    // test that pins in a bug.
    for r in &rows {
        assert!(
            r.model.is_some(),
            "{} {} produced no body at all — every offered race must assemble something",
            r.label,
            r.gender_name()
        );
        assert!(
            r.scale > 0.0,
            "{} {} has a non-positive display scale {}",
            r.label,
            r.gender_name(),
            r.scale
        );
    }
}

/// What the client actually ships under `figures/Mskins`, for the body-cloth slots.
///
/// The matrix above splits perfectly by realm — every Albion body binds cloth, no Midgard or
/// Hibernian one does — which is a naming question, not a rendering question. This prints the
/// real member namespace so the binder's candidate names can be derived from it instead of
/// guessed.
#[test]
fn mskins_body_cloth_namespace() {
    use std::collections::{BTreeMap, BTreeSet};

    let root = root();
    let dir = root.join("figures/Mskins");
    assert!(dir.is_dir(), "no figures/Mskins in the client tree");

    let mut archives: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read figures/Mskins")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("mpk")))
        .collect();
    archives.sort();
    assert!(
        !archives.is_empty(),
        "figures/Mskins holds no .mpk archives"
    );

    let slots = ["body", "lbody", "arms", "gloves", "legs", "boots"];
    let mut by_slot: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut total = 0usize;
    for path in &archives {
        let Ok(names) = caer_assets::list_names(path) else {
            continue;
        };
        for n in names {
            total += 1;
            let low = n.to_ascii_lowercase();
            let Some(stem) = low.strip_suffix(".dds") else {
                continue;
            };
            for slot in slots {
                // `body01_<something>` — the suffix after the slot's `NN` index is the axis the
                // binder has to get right.
                if let Some(rest) = stem.strip_prefix(slot) {
                    if rest.len() > 2 && rest.as_bytes()[0].is_ascii_digit() {
                        if let Some(suffix) = rest[2..].strip_prefix('_') {
                            by_slot.entry(slot).or_default().insert(suffix.to_string());
                        }
                    }
                    break;
                }
            }
        }
    }
    println!(
        "\nindexed {total} Mskins members across {} archives",
        archives.len()
    );
    for (slot, suffixes) in &by_slot {
        let mut sample: Vec<&String> = suffixes.iter().take(24).collect();
        sample.sort();
        println!(
            "  {slot:<7} {:>4} distinct suffixes: {}",
            suffixes.len(),
            sample
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    // The binder asks for `<slot>01_<Realm>_<set>.dds`. Show which of those actually exist.
    println!("\ndoes the binder's guessed name exist?");
    for realm in ["Albion", "Midgard", "Hibernia"] {
        for set in ["cloth", "leather"] {
            let want = format!("{}_{}", realm.to_ascii_lowercase(), set);
            let hits: Vec<&str> = by_slot
                .iter()
                .filter(|(_, s)| s.contains(&want))
                .map(|(k, _)| *k)
                .collect();
            println!(
                "  {realm:<9} {set:<8} -> {}",
                if hits.is_empty() {
                    "NO SLOT HAS IT".to_string()
                } else {
                    hits.join(", ")
                }
            );
        }
    }
}

/// What `fig3map.csv` offers per body part beyond the variant we take.
///
/// `FigureModels` keeps the FIRST non-empty variant for each part, which is the bare body. The
/// data audit calls columns 2..25 "armour/cloth variant — create-preview clothes", so if the
/// create screen is meant to show a dressed body the clothes are a different PART, not a texture
/// painted onto the naked one. This prints the real fan-out so that claim can be checked instead
/// of assumed.
#[test]
fn fig3map_variant_fanout_per_part() {
    let root = root();
    let members = caer_assets::open(root.join("gamedata.mpk")).expect("gamedata.mpk");
    let table = members
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case("fig3map.csv"))
        .map(|m| String::from_utf8_lossy(&m.data).into_owned())
        .expect("fig3map.csv in gamedata.mpk");

    // Part index -> what a base body calls it. Matches figures::BASE_BODY_PARTS.
    let part_name = |p: u32| match p {
        0 => "head",
        1 => "body",
        2 => "lbody",
        3 => "legs",
        4 => "boots",
        5 => "arms",
        6 => "gloves",
        7 => "cloak",
        8 => "hair",
        9 => "helm",
        _ => "?",
    };

    println!("\nfig3map variant fan-out (columns 3.. are the variants):");
    let mut shown = 0usize;
    for line in table.lines() {
        let cols: Vec<&str> = line.split(',').map(str::trim).collect();
        if cols.len() < 5 {
            continue;
        }
        let Ok(fig) = cols[0].parse::<u32>() else {
            continue;
        };
        let Ok(part) = cols[1].parse::<u32>() else {
            continue;
        };
        // Two representative figures: one Albion, one Hibernian, chosen by their Head description.
        let want = table
            .lines()
            .find(|l| {
                let c: Vec<&str> = l.split(',').map(str::trim).collect();
                c.len() > 2
                    && c.first().and_then(|s| s.parse::<u32>().ok()) == Some(fig)
                    && c.get(1).and_then(|s| s.parse::<u32>().ok()) == Some(0)
                    && (c[2].starts_with("Briton Male") || c[2].starts_with("Firbolg Male"))
            })
            .is_some();
        if !want {
            continue;
        }
        let variants: Vec<&str> = cols[3..]
            .iter()
            .copied()
            .filter(|s| !s.is_empty() && *s != "0")
            .collect();
        println!(
            "  fig {fig:>4} part {part} ({:<6}) desc {:<28} {} variant(s): {}",
            part_name(part),
            cols[2],
            variants.len(),
            variants.join(" ")
        );
        shown += 1;
        if shown > 24 {
            break;
        }
    }
    assert!(
        shown > 0,
        "no representative figure rows found in fig3map.csv"
    );
}

/// Which texture each base-body part asks for, and where in the client that file actually lives.
///
/// This is the measurement that separates the two candidate causes of a white limb: the NIF names
/// no texture at all (an asset fact we must honour), or it names one we fail to find (our search
/// path). `avatar_texture` looks only in `figures/Mskins/*.mpk`, so if the referenced DDS ships
/// anywhere else every race outside that archive family renders bare.
#[test]
fn base_body_texture_references_resolve() {
    use std::collections::{BTreeMap, BTreeSet, HashMap};

    let root = root();
    let figures = caer_assets::figures::FigureModels::load(root.join("gamedata.mpk"))
        .expect("fig3 tables from gamedata.mpk");

    // Everything Mskins can answer.
    let mut mskins: BTreeSet<String> = BTreeSet::new();
    if let Ok(rd) = std::fs::read_dir(root.join("figures/Mskins")) {
        for e in rd.flatten() {
            if let Ok(names) = caer_assets::list_names(e.path()) {
                mskins.extend(names.into_iter().map(|n| n.to_ascii_lowercase()));
            }
        }
    }
    // Everything the loose client tree can answer, by bare file name.
    let mut loose: HashMap<String, String> = HashMap::new();
    for dir in ["figures", "items", "effects", "pregame", "zones/Nifs"] {
        if let Ok(rd) = std::fs::read_dir(root.join(dir)) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_ascii_lowercase();
                if n.ends_with(".dds") || n.ends_with(".tga") {
                    loose.entry(n).or_insert_with(|| dir.to_string());
                }
            }
        }
    }
    println!(
        "\nMskins members: {}   loose dds/tga indexed: {}",
        mskins.len(),
        loose.len()
    );

    let mut where_found: BTreeMap<&str, usize> = BTreeMap::new();
    let mut unresolved: Vec<String> = Vec::new();
    let mut no_reference: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for (race, label, wire) in legal_combinations() {
        let gender = caer_protocol::overview::fig3_gender_from_db(wire);
        for part in figures.base_body(race, gender) {
            let Ok(Some(bytes)) =
                caer_assets::open_member(root.join(part.archive_path()), &part.nif_member())
            else {
                continue;
            };
            let Ok(model) = caer_assets::nif::read_model(&bytes) else {
                continue;
            };
            for p in &model.parts {
                checked += 1;
                let Some(reference) = p.texture.as_deref() else {
                    no_reference.push(format!("{label}/{}", part.filename));
                    *where_found.entry("NIF names no texture").or_default() += 1;
                    continue;
                };
                let base = reference
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(reference)
                    .to_ascii_lowercase();
                let stem = base
                    .rsplit_once('.')
                    .map_or(base.clone(), |(s, _)| s.into());
                let dds = format!("{stem}.dds");
                let tga = format!("{stem}.tga");
                if mskins.contains(&dds) || mskins.contains(&tga) {
                    *where_found.entry("figures/Mskins").or_default() += 1;
                } else if let Some(dir) = loose.get(&dds).or_else(|| loose.get(&tga)) {
                    // Reachable in the tree, but not from where `avatar_texture` looks.
                    *where_found
                        .entry(match dir.as_str() {
                            "figures" => "loose figures/ (NOT SEARCHED)",
                            "items" => "items/ (NOT SEARCHED)",
                            "effects" => "effects/ (NOT SEARCHED)",
                            "pregame" => "pregame/ (NOT SEARCHED)",
                            _ => "zones/ (NOT SEARCHED)",
                        })
                        .or_default() += 1;
                } else {
                    *where_found.entry("nowhere we indexed").or_default() += 1;
                    unresolved.push(format!("{label} {} -> {stem}", part.filename));
                }
            }
        }
    }

    println!("\nbase-body texture references across every legal body ({checked} mesh parts):");
    for (place, n) in &where_found {
        println!("  {n:>5}  {place}");
    }
    if !unresolved.is_empty() {
        println!("\nunresolved anywhere (first 25 of {}):", unresolved.len());
        for u in unresolved.iter().take(25) {
            println!("  {u}");
        }
    }
    if !no_reference.is_empty() {
        println!(
            "\nparts whose NIF names no texture at all: {} (first 12: {})",
            no_reference.len(),
            no_reference
                .iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    assert!(checked > 0, "no base-body mesh parts were inspected");
}

/// Does the client's own skin-tint table answer for every legal body?
///
/// The base-body meshes carry no texture reference, so their colour is not a lookup we are
/// failing — it is a tint the client stores in `fig3skincolormap.csv` -> `fig3tints.csv`. If that
/// resolves for every race and gender then a white body is our binder ignoring the data, not the
/// data being absent, and the fix has exactly one owner.
#[test]
fn skin_tint_resolves_for_every_legal_body() {
    let root = root();
    let look = caer_assets::fig3_look::Fig3Look::load(root.join("gamedata.mpk"))
        .expect("fig3 look tables from gamedata.mpk");
    println!(
        "\nfig3 look tables: {} tints, {} face entries",
        look.tint_count(),
        look.face_count()
    );

    let mut missing: Vec<String> = Vec::new();
    println!(
        "\n{:<14} {:<7} {:<14} {:<14}",
        "race", "gender", "skin tone 1", "face skin id"
    );
    for (race, label, wire) in legal_combinations() {
        let gender = caer_protocol::overview::fig3_gender_from_db(wire);
        let Some(name) = caer_assets::figures::FigureModels::race_name(race) else {
            missing.push(format!("{label} (no race name)"));
            continue;
        };
        let rgb = look.skin_rgb(name, gender, 1);
        let face = look.face_skin_id(name, gender, 1);
        println!(
            "{:<14} {:<7} {:<14} {:<14}",
            label,
            if wire == 0 { "male" } else { "female" },
            rgb.map_or("-".into(), |c| format!("{},{},{}", c[0], c[1], c[2])),
            face.map_or("-".into(), |f| f.to_string()),
        );
        if rgb.is_none() {
            missing.push(format!(
                "{label} {}",
                if wire == 0 { "male" } else { "female" }
            ));
        }
    }
    if missing.is_empty() {
        println!("\nevery legal body has a skin tint in the client's own tables.");
    } else {
        println!(
            "\nno skin tint for {} combination(s): {}",
            missing.len(),
            missing.join(", ")
        );
    }

    // Pinned exceptions, not a threshold. The three Minotaur races (19/20/21, all named
    // "Minotaur") have no fig3 skin tint because they do not use the fig3 path at all — they
    // render through `path=living`, which ledger B7 established after an instrument reported them
    // blank and a capture showed them correct.
    //
    // The gate is that the exception SET does not grow: a playable race losing its tint fails
    // here. Asserting `missing.is_empty()` would be false today and asserting nothing was the
    // defect this repairs.
    let unexpected: Vec<&String> = missing
        .iter()
        .filter(|m| !m.starts_with("Minotaur"))
        .collect();
    assert!(
        unexpected.is_empty(),
        "a body outside the documented fig3-exempt races has no skin tint: {unexpected:?}"
    );
    assert_eq!(
        missing.len(),
        3,
        "expected exactly the three Minotaur races to be fig3-exempt, got {missing:?} — if a \
         Minotaur gained a tint this pin is stale and should be tightened, not widened"
    );
}

/// Where the body skin actually comes from, if not the NIF and not a flat tint.
///
/// Tone 1 is `255,255,255` for every race, so the tint is a modulator over something, not the
/// colour itself. `fig3facemap` gives a per-race `skins.csv` id for the FACE; this looks for the
/// body's equivalent in the same table rather than continuing to guess file names.
#[test]
fn skins_table_rows_for_base_body_parts() {
    let root = root();
    let members = caer_assets::open(root.join("gamedata.mpk")).expect("gamedata.mpk");
    let mut names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
    names.sort_unstable();
    println!("\ngamedata.mpk members ({}):", names.len());
    for chunk in names.chunks(6) {
        println!("  {}", chunk.join("  "));
    }

    let skins = members
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case("skins.csv"))
        .map(|m| String::from_utf8_lossy(&m.data).into_owned())
        .expect("skins.csv");

    // The face ids the look table hands out, so we can see the shape of a row we already trust.
    let look = caer_assets::fig3_look::Fig3Look::load(root.join("gamedata.mpk")).expect("look");
    let briton_face = look.face_skin_id("Briton", 1, 1).expect("Briton male face");
    println!("\nBriton male face skin id = {briton_face}");

    // Print that row, then every row whose name looks like a base-body part for the same race.
    println!("\nskins.csv rows of interest:");
    let mut shown = 0usize;
    for line in skins.lines() {
        let cols: Vec<&str> = line.split(',').map(str::trim).collect();
        let Some(id) = cols.first().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let joined = cols.join(",").to_ascii_lowercase();
        let is_face_row = id == u32::from(briton_face);
        let is_body_row = joined.contains("bri_m_") || joined.contains("body01_bc_m");
        if is_face_row || is_body_row {
            println!("  {line}");
            shown += 1;
            if shown > 30 {
                break;
            }
        }
    }
    assert!(
        shown > 0,
        "no skins.csv row matched the Briton male face id or body tokens"
    );
}

/// The two tables the avatar binder should be reading and currently is not (or misreads).
#[test]
fn facemap_and_mskins_table_shape() {
    let root = root();
    let members = caer_assets::open(root.join("gamedata.mpk")).expect("gamedata.mpk");
    let text = |name: &str| -> String {
        members
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(name))
            .map(|m| String::from_utf8_lossy(&m.data).into_owned())
            .unwrap_or_default()
    };

    println!("\n--- fig3facemap.csv rows whose description mentions Briton ---");
    let facemap = text("fig3facemap.csv");
    for (n, line) in facemap.lines().enumerate() {
        if line.to_ascii_lowercase().contains("briton") {
            println!("  line {n:>4}: {line}");
        }
    }

    println!("\n--- mskins.csv: first 3 lines, then rows mentioning a base-body slot ---");
    let mskins = text("mskins.csv");
    assert!(
        !mskins.is_empty(),
        "mskins.csv is in gamedata.mpk per the member list"
    );
    for line in mskins.lines().take(3) {
        println!("  {line}");
    }
    // What naked-body skins exist at all, and how are they named? The base-body meshes carry no
    // texture reference, so their skin must be named by a table — this shows the vocabulary both
    // candidate tables actually use.
    use std::collections::BTreeMap;
    let vocabulary = |csv: &str, name_col: usize| -> (usize, BTreeMap<String, Vec<String>>) {
        let mut by_kind: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut total = 0usize;
        for line in csv.lines() {
            let cols: Vec<&str> = line.split(',').map(str::trim).collect();
            if cols.len() <= name_col || cols[0].parse::<u32>().is_err() {
                continue;
            }
            total += 1;
            let name = cols[name_col].to_ascii_lowercase();
            for kind in [
                "lbody", "body", "head", "hair", "arms", "gloves", "legs", "boots", "face", "cloak",
            ] {
                if name.contains(kind) {
                    by_kind
                        .entry(kind.to_string())
                        .or_default()
                        .push(format!("{}={}", cols[0], cols[name_col]));
                    break;
                }
            }
        }
        (total, by_kind)
    };

    for (label, csv) in [("mskins.csv", &mskins), ("skins.csv", &text("skins.csv"))] {
        let (total, by_kind) = vocabulary(csv, 2);
        println!("\n  {label}: {total} rows; base-body vocabulary");
        for (kind, names) in &by_kind {
            let sample: Vec<&str> = names.iter().take(5).map(String::as_str).collect();
            println!(
                "    {kind:<7} {:>5} rows  e.g. {}",
                names.len(),
                sample.join("  ")
            );
        }
    }

    // Retail dresses each race in its OWN default clothing — a Highlander wears a tartan kilt,
    // not the generic cloth every race currently gets. Find the vocabulary for that.
    // `<Race>_ss_<Slot>` is the per-race STARTING SET. This is the table the create screen dresses
    // from — Briton_ss_Body, SaracenM_ss_gloves, and so on.
    println!("\n  starting-set (_ss_) rows, by table:");
    for (label, csv) in [("mskins", &mskins), ("skins", &text("skins.csv"))] {
        let mut rows: Vec<String> = Vec::new();
        for line in csv.lines() {
            let cols: Vec<&str> = line.split(',').map(str::trim).collect();
            if cols.len() < 5 || cols[0].parse::<u32>().is_err() {
                continue;
            }
            if cols[2].to_ascii_lowercase().contains("_ss_") {
                rows.push(format!("{}={} [arch {}]", cols[0], cols[2], cols[4]));
            }
        }
        println!("    {label}: {} rows", rows.len());
        for r in &rows {
            println!("      {r}");
        }
    }

    println!("\n  lbody / hair texture availability:");
    for needle in [
        "lbody01_hig",
        "cthlbody",
        "hig_m_hair01",
        "inc_m_hair01",
        "lbody01_",
    ] {
        let mut hits: Vec<String> = Vec::new();
        for (label, csv) in [("mskins", &mskins), ("skins", &text("skins.csv"))] {
            for line in csv.lines() {
                let cols: Vec<&str> = line.split(',').map(str::trim).collect();
                if cols.len() < 3 || cols[0].parse::<u32>().is_err() {
                    continue;
                }
                if cols[2].to_ascii_lowercase().contains(needle) {
                    hits.push(format!("{label}:{}", cols[2]));
                }
            }
        }
        println!(
            "    {needle:<14} {:>3} hits  {}",
            hits.len(),
            hits.iter().take(8).cloned().collect::<Vec<_>>().join("  ")
        );
    }

    println!("\n  race-flavoured clothing candidates:");
    for needle in [
        "tartan", "plaid", "kilt", "highland", "hig_", "bri_m_b", "briton", "norse", "celt",
    ] {
        let mut hits: Vec<String> = Vec::new();
        for (label, csv) in [("mskins", &mskins), ("skins", &text("skins.csv"))] {
            for line in csv.lines() {
                let cols: Vec<&str> = line.split(',').map(str::trim).collect();
                if cols.len() < 3 || cols[0].parse::<u32>().is_err() {
                    continue;
                }
                if cols[2].to_ascii_lowercase().contains(needle) {
                    hits.push(format!("{label}:{}={}", cols[0], cols[2]));
                }
            }
        }
        println!(
            "    {needle:<10} {:>4} hits  {}",
            hits.len(),
            hits.iter().take(6).cloned().collect::<Vec<_>>().join("  ")
        );
    }

    // The cloth set specifically — "cth" is the starter clothing the create screen dresses a new
    // character in, and it is the vocabulary a real binder has to speak.
    println!("\n  mskins.csv cloth rows (id = name = archive):");
    let mut cloth: Vec<(u32, String, String)> = Vec::new();
    for line in mskins.lines() {
        let cols: Vec<&str> = line.split(',').map(str::trim).collect();
        if cols.len() < 5 {
            continue;
        }
        let Ok(id) = cols[0].parse::<u32>() else {
            continue;
        };
        let name = cols[2].to_ascii_lowercase();
        if name.starts_with("cth") && !name.contains("bump") && !name.contains("gloss") {
            cloth.push((id, cols[2].to_string(), cols[4].to_string()));
        }
    }
    cloth.sort();
    println!("  {} cloth rows (bump/gloss excluded)", cloth.len());
    for (id, name, arch) in cloth.iter().take(40) {
        println!("    {id:>5}  {name:<34} archive {arch}");
    }
}

fn summarise(rows: &[&Row]) -> String {
    if rows.is_empty() {
        return "none".into();
    }
    let names: Vec<String> = rows
        .iter()
        .map(|r| format!("{} {}", r.label, r.gender_name()))
        .collect();
    format!("{} — {}", rows.len(), names.join(", "))
}

#[test]
fn idle_animation_changes_the_pose() {
    let root = root();
    // SAFETY: single-threaded test entry; `EntityModels::load` reads the client root from env.
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu =
        pollster::block_on(Gpu::new_headless(CELL_W, CELL_H, 60_000.0)).expect("gpu init");
    let mut models = EntityModels::load().expect("entity models from the retail tree");
    caer_render::atmosphere::publish(caer_render::atmosphere::Atmosphere::load_for_region(
        &root,
        "sky_default",
    ));

    let mut checked = 0usize;
    let mut frozen: Vec<String> = Vec::new();
    println!(
        "\n{:<14} {:<7} {:>9} {:>12}",
        "race", "gender", "clip", "pixels moved"
    );

    for (race, label, wire) in legal_combinations() {
        let gender = caer_protocol::overview::fig3_gender_from_db(wire);
        let Some(model) = models.ensure_avatar(
            &mut gpu,
            race,
            gender,
            caer_protocol::customization::Customization::default(),
            None,
        ) else {
            continue;
        };
        let Some(dur) = models.skinned_rig(model).map(|r| r.clip.duration) else {
            continue; // unrigged bodies are covered by the matrix, not here
        };
        if dur <= 0.0 {
            continue;
        }
        let scale = models.race_display_scale(race, gender);
        let height = models.model_height(model).unwrap_or(70.0);
        let back = height * 1.35 / (30.0_f32.to_radians()).tan();
        let mut cam = Camera::new(
            Vec3::new(0.0, -back, height * 0.55),
            Vec3::new(0.0, 0.0, height * 0.5),
            Vec3::ZERO,
            CELL_W as f32 / CELL_H as f32,
        );
        cam.ensure_far(60_000.0);
        gpu.set_view_proj(cam.view_proj().to_cols_array_2d());

        // Two samples a half-cycle apart: the largest separation the clip offers.
        let mut frames = Vec::new();
        for t in [0.0_f32, dur * 0.5] {
            gpu.clear_skinned_instances();
            gpu.clear_entity_instances();
            let Some(rig) = models.skinned_rig(model) else {
                break;
            };
            let palettes = caer_render::anim_skin::build_palettes(
                rig,
                &[caer_render::anim_skin::UniquePaletteJob {
                    loco: caer_render::entities::Loco::Idle,
                    t,
                    blend: None,
                }],
            );
            if palettes.is_empty() {
                break;
            }
            gpu.update_skinned_instances(
                model,
                &[caer_render::gpu::SkinnedInstance {
                    pos_yaw: [0.0, 0.0, 0.0, std::f32::consts::PI],
                    scale,
                    palette_base: 0.0,
                }],
                &palettes,
            );
            frames.push(
                gpu.render_to_rgba_pass(0, FramePass::PreWorldScene)
                    .expect("pose render"),
            );
        }
        if frames.len() != 2 {
            continue;
        }
        let moved = frames[0]
            .chunks_exact(4)
            .zip(frames[1].chunks_exact(4))
            .filter(|(a, b)| {
                a[0].abs_diff(b[0]) > 8 || a[1].abs_diff(b[1]) > 8 || a[2].abs_diff(b[2]) > 8
            })
            .count();
        let g = if wire == 0 { "male" } else { "female" };
        println!("{label:<14} {g:<7} {dur:>8.1}s {moved:>12}");
        checked += 1;
        if moved == 0 {
            frozen.push(format!("{label} {g}"));
        }
    }

    assert!(checked > 20, "only {checked} rigged bodies were sampled");
    assert!(
        frozen.is_empty(),
        "{} body/bodies render an IDENTICAL pose at t=0 and t=half-cycle — the idle clip is \
         loaded but not animating: {}",
        frozen.len(),
        frozen.join(", ")
    );
}

#[test]
fn render_every_body_to_one_sheet() {
    let root = root();
    // SAFETY: single-threaded test entry; `EntityModels::load` reads the client root from env.
    std::env::set_var("CAER_CLIENT", &root);

    let mut gpu =
        pollster::block_on(Gpu::new_headless(CELL_W, CELL_H, 60_000.0)).expect("gpu init");
    let mut models = EntityModels::load().expect("entity models from the retail tree");
    caer_render::atmosphere::publish(caer_render::atmosphere::Atmosphere::load_for_region(
        &root,
        "sky_default",
    ));

    let combos = legal_combinations();
    let rows = combos.len().div_ceil(COLS as usize) as u32;
    let (sheet_w, sheet_h) = (COLS * CELL_W, rows * CELL_H);
    let mut sheet = vec![0u8; (sheet_w * sheet_h * 4) as usize];

    let mut blank: Vec<String> = Vec::new();
    let mut report: Vec<(String, f64, &'static str, bool)> = Vec::new();

    for (i, (race, label, wire)) in combos.iter().enumerate() {
        let gender = caer_protocol::overview::fig3_gender_from_db(*wire);
        let model = models.ensure_avatar(
            &mut gpu,
            *race,
            gender,
            caer_protocol::customization::Customization::default(),
            None,
        );
        let info = models.last_avatar_stand();
        let (path, rigged) = (
            info.path,
            model.and_then(|m| models.skinned_rig(m)).is_some(),
        );

        gpu.clear_skinned_instances();
        gpu.clear_entity_instances();

        if let Some(model) = model {
            let scale = models.race_display_scale(*race, gender);
            let height = models.model_height(model).unwrap_or(70.0);
            // Frame the body head to toe with a little air, straight on.
            let back = height * 1.35 / (30.0_f32.to_radians()).tan();
            let mut cam = Camera::new(
                Vec3::new(0.0, -back, height * 0.55),
                Vec3::new(0.0, 0.0, height * 0.5),
                Vec3::ZERO,
                CELL_W as f32 / CELL_H as f32,
            );
            cam.ensure_far(60_000.0);
            gpu.set_view_proj(cam.view_proj().to_cols_array_2d());

            let inst = caer_render::gpu::SkinnedInstance {
                pos_yaw: [0.0, 0.0, 0.0, std::f32::consts::PI],
                scale,
                palette_base: 0.0,
            };
            let palettes = models.skinned_rig(model).map(|rig| {
                caer_render::anim_skin::build_palettes(
                    rig,
                    &[caer_render::anim_skin::UniquePaletteJob {
                        loco: caer_render::entities::Loco::Idle,
                        t: 0.0,
                        blend: None,
                    }],
                )
            });
            match palettes {
                Some(p) if !p.is_empty() => gpu.update_skinned_instances(model, &[inst], &p),
                _ => gpu.update_entity_instances(
                    model,
                    &[[
                        inst.pos_yaw[0],
                        inst.pos_yaw[1],
                        inst.pos_yaw[2],
                        inst.pos_yaw[3],
                        inst.scale,
                    ]],
                ),
            }
        }

        let cell = gpu
            .render_to_rgba_pass(0, FramePass::PreWorldScene)
            .expect("cell render");
        let cov = coverage(&cell);
        if cov < 0.01 {
            blank.push(format!("{label} {}", if *wire == 0 { "m" } else { "f" }));
        }
        report.push((
            format!("{label} {}", if *wire == 0 { "male" } else { "female" }),
            cov,
            path,
            rigged,
        ));

        // Blit into the sheet.
        let (cx, cy) = ((i as u32 % COLS) * CELL_W, (i as u32 / COLS) * CELL_H);
        for y in 0..CELL_H {
            let src = (y * CELL_W * 4) as usize;
            let dst = (((cy + y) * sheet_w + cx) * 4) as usize;
            sheet[dst..dst + (CELL_W * 4) as usize]
                .copy_from_slice(&cell[src..src + (CELL_W * 4) as usize]);
        }
    }

    // `CARGO_TARGET_TMPDIR` is Cargo's own scratch for integration tests. A relative "target/"
    // resolves against the crate directory, not the workspace target, so every run dropped a PNG
    // into `crates/caer-render/target/` inside the source tree — gitignored, so it accumulated
    // silently.
    let out = std::env::var("CAER_SHEET_OUT")
        .unwrap_or_else(|_| format!("{}/avatar_sheet.png", env!("CARGO_TARGET_TMPDIR")));
    if let Some(parent) = std::path::Path::new(&out).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = std::fs::File::create(&out).expect("create sheet");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), sheet_w, sheet_h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .expect("png header")
        .write_image_data(&sheet)
        .expect("png data");

    println!(
        "\ncontact sheet: {out}  ({sheet_w}x{sheet_h}, {} bodies)",
        combos.len()
    );
    println!(
        "{:<22} {:>9} {:<7} {}",
        "body", "coverage", "path", "rigged"
    );
    for (name, cov, path, rigged) in &report {
        println!("{name:<22} {:>8.1}% {path:<7} {rigged}", cov * 100.0);
    }

    assert!(
        blank.is_empty(),
        "{} body/bodies rendered blank: {}",
        blank.len(),
        blank.join(", ")
    );
}

#[test]
fn worn_armour_changes_the_bound_textures() {
    let root = root();
    // SAFETY: single-threaded test entry; `EntityModels::load` reads the client root from env.
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 60_000.0)).expect("gpu init");
    let mut models = EntityModels::load().expect("entity models");

    // Diagnose the table before blaming the renderer. `starter_armor_model` demands one object
    // with body+arms+legs+boots all non-zero; if none exists the equipment path has nothing to
    // exercise and "eq_bound = 0" is the table's answer, not a binder bug.
    let table = caer_assets::pskins::ObjectSkins::load(root.join("gamedata.mpk"))
        .expect("objects.csv + pskins.csv");
    let mut with_any = 0usize;
    let mut with_full = 0usize;
    let mut best: Option<(u16, usize)> = None;
    for id in 0u16..=u16::MAX {
        let Some(o) = table.object(id) else { continue };
        let filled = [o.body, o.arms, o.gloves, o.legs, o.boots, o.lbody]
            .iter()
            .filter(|v| **v != 0)
            .count();
        if filled > 0 {
            with_any += 1;
        }
        if o.body != 0 && o.arms != 0 && o.legs != 0 && o.boots != 0 {
            with_full += 1;
        }
        if best.is_none_or(|(_, n)| filled > n) {
            best = Some((id, filled));
        }
    }
    println!(
        "\nobjects.csv armour table: {with_any} objects declare at least one skin slot, \
         {with_full} declare the full body+arms+legs+boots set"
    );

    let armour_object = match models.starter_armor_model().or(best.map(|(id, _)| id)) {
        Some(id) => id,
        None => {
            println!(
                "no object in the table declares ANY armour skin — the equipment path has \
                      nothing to bind, and that is a data/parse fact, not a renderer bug"
            );
            return;
        }
    };
    println!("\nstarter armour object id {armour_object}");
    println!(
        "{:<12} {:<7} {:>9} {:>9} {:>8}",
        "race", "gender", "naked tex", "armed tex", "changed"
    );

    let mut dressed_any = false;
    let mut unchanged: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for (race, label, wire) in legal_combinations() {
        let gender = caer_protocol::overview::fig3_gender_from_db(wire);
        let Some(_) = models.ensure_avatar(
            &mut gpu,
            race,
            gender,
            caer_protocol::customization::Customization::default(),
            None,
        ) else {
            continue;
        };
        let naked: BTreeSet<String> = models
            .last_avatar_stand()
            .bound_textures
            .iter()
            .map(|(_, t)| t.clone())
            .collect();

        // Dress every armour slot the starter object covers.
        let equip = caer_protocol::equipment::EquipmentUpdate {
            object_id: 1,
            active_weapon_slots: 0,
            speed: 0,
            cloak_hidden: false,
            helm_hidden: false,
            hood_up: false,
            active_quiver: 0,
            items: (0x15..=0x1a)
                .map(|slot| caer_protocol::equipment::VisibleItem {
                    slot,
                    model: armour_object,
                    extension: Some(0),
                    texture: None,
                    effect: None,
                    new_emblem: false,
                })
                .collect(),
        };
        let Some(_) = models.ensure_avatar(
            &mut gpu,
            race,
            gender,
            caer_protocol::customization::Customization::default(),
            Some(&equip),
        ) else {
            continue;
        };
        let info = models.last_avatar_stand();
        let armed: BTreeSet<String> = info.bound_textures.iter().map(|(_, t)| t.clone()).collect();
        let eq_bound = info.eq_bound;

        checked += 1;
        if eq_bound > 0 {
            dressed_any = true;
        }
        let g = if wire == 0 { "male" } else { "female" };
        println!(
            "{label:<12} {g:<7} {:>9} {:>9} {:>8}",
            naked.len(),
            armed.len(),
            if armed == naked { "no" } else { "YES" }
        );
        if armed == naked {
            unchanged.push(format!("{label} {g}"));
        }
    }

    assert!(checked > 20, "only {checked} bodies were dressed");
    // Reported, not asserted: if the client's starter object legitimately maps to the same skins
    // the naked body already wears, "unchanged" is the truth and an assertion would be inventing a
    // requirement. What IS assertable is that the equipment path did something for someone.
    if !dressed_any {
        println!(
            "\nNO body bound a single equipment texture — the objects.csv -> pskins.csv path is \
             inert. {} of {checked} bodies were byte-identical dressed and naked.",
            unchanged.len()
        );
    } else {
        println!(
            "\nequipment textures bound on at least one body; {} unchanged",
            unchanged.len()
        );
    }
}

/// **A8 — the character-creation variant axes are populated.**
///
/// Creation offers 7 face types and a set of hair colours per race. The renderer hardcodes both to
/// `1`, so nothing has ever checked that the other values resolve to *different* art. If they all
/// collapse to one id, the selectors on the create screen would be decorative.
///
/// Fails if a race offers only one distinct face across all 7 slots — that is a table we are
/// reading wrong, not a client that ships one face.
#[test]
fn creation_variant_axes_offer_distinct_art() {
    let root = root();
    let look = caer_assets::fig3_look::Fig3Look::load(root.join("gamedata.mpk")).expect("look");

    println!(
        "\n{:<12} {:<7} {:>12} {:>12} {:>12}",
        "race", "gender", "faces 1..7", "hair 1..8", "tones 1..8"
    );
    let mut single_face: Vec<String> = Vec::new();
    let mut rows = 0usize;

    for id in 1..=18u8 {
        let Some(name) = caer_assets::figures::FigureModels::race_name(id) else {
            continue;
        };
        for (g, gname) in [(1u8, "male"), (2, "female")] {
            let faces: BTreeSet<u16> = (1..=7)
                .filter_map(|f| look.face_skin_id(name, g, f))
                .collect();
            let hair: BTreeSet<u16> = (1..=8)
                .filter_map(|c| look.hair_skin_id(name, g, c))
                .collect();
            let tones: BTreeSet<[u8; 3]> =
                (1..=8).filter_map(|t| look.skin_rgb(name, g, t)).collect();
            println!(
                "{name:<12} {gname:<7} {:>12} {:>12} {:>12}",
                faces.len(),
                hair.len(),
                tones.len()
            );
            rows += 1;
            if faces.len() <= 1 {
                single_face.push(format!("{name} {gname}"));
            }
        }
    }

    assert!(rows >= 30, "only {rows} race/gender rows were swept");
    assert!(
        single_face.is_empty(),
        "{} race/gender rows offer one face across all 7 slots — the facemap is being read wrong: \
         {}",
        single_face.len(),
        single_face.join(", ")
    );
}

/// **A9 — skin and hair tints resolve to real colours, and whether they reach a draw.**
///
/// `fig3skincolormap` → `fig3tints` gives a per-race/gender/tone RGB. Tone 1 is `255,255,255` for
/// every race — a neutral multiplier — so an implementation that ignores tints entirely looks
/// identical to a correct one at the default. The other tones are where the difference lives.
///
/// Fails if the tone axis is flat (every tone the same colour), which would mean the table is
/// misparsed. It also reports, without asserting, whether anything consumes the value — the
/// renderer currently does not, and that gap should be visible rather than silent.
#[test]
fn skin_tint_axis_is_not_flat() {
    let root = root();
    let look = caer_assets::fig3_look::Fig3Look::load(root.join("gamedata.mpk")).expect("look");

    let mut flat: Vec<String> = Vec::new();
    let mut rows = 0usize;
    println!("\n{:<12} {:<7} {}", "race", "gender", "tones 1..4");
    for id in 1..=18u8 {
        let Some(name) = caer_assets::figures::FigureModels::race_name(id) else {
            continue;
        };
        for (g, gname) in [(1u8, "male"), (2, "female")] {
            let tones: Vec<[u8; 3]> = (1..=4).filter_map(|t| look.skin_rgb(name, g, t)).collect();
            if tones.is_empty() {
                continue;
            }
            let distinct: BTreeSet<[u8; 3]> = tones.iter().copied().collect();
            println!(
                "{name:<12} {gname:<7} {}",
                tones
                    .iter()
                    .map(|c| format!("{},{},{}", c[0], c[1], c[2]))
                    .collect::<Vec<_>>()
                    .join("  ")
            );
            rows += 1;
            if distinct.len() <= 1 {
                flat.push(format!("{name} {gname}"));
            }
        }
    }

    assert!(rows >= 30, "only {rows} rows carried any tone at all");
    assert!(
        flat.is_empty(),
        "{} race/gender rows resolve one colour across every tone — misparsed table: {}",
        flat.len(),
        flat.join(", ")
    );
    println!(
        "\nNOTE: these values are resolved but NOT applied to any draw. Until a binder consumes \
         them, every character renders at the tone-1 neutral regardless of the wire value."
    );
}

/// **Firbolg's authored skeleton is present, parseable, and the one its parts name.**
///
/// Reports each base-body part's own bone names against the race skeleton we load for it. A part
/// whose bones are absent from that skeleton would be bound to a different one; the overlap keeps
/// the user's "upsized Celt" hypothesis measurable instead of guessing from a screenshot.
/// Scanning the `sfig*` archives must use the first copy that PARSES, and the parser must not throw
/// away an authored skeleton just because its file also carries a lighting controller. Firbolg
/// ships the same 129-bone rig in two archives; `sfig001` includes the Max lighting blocks and
/// `sfig002` is the stripped copy. Both must resolve to the same bind transforms.
#[test]
fn the_firbolg_skeleton_copies_parse_and_agree() {
    let root = root();
    let member = caer_assets::figures::FigureModels::skeleton_member(10, 1)
        .expect("Firbolg male names a skeleton member");
    let archives = caer_assets::figures::skeleton_archives(&root).expect("fig3 skeleton archives");

    let holders: Vec<(PathBuf, bool)> = archives
        .iter()
        .filter_map(|a| {
            let bytes = caer_assets::open_member(a, &member).ok()??;
            Some((a.clone(), caer_assets::nif::read_skeleton(&bytes).is_ok()))
        })
        .collect();
    for (a, ok) in &holders {
        println!(
            "  {:<14} {member}  {}",
            a.file_name().unwrap_or_default().to_string_lossy(),
            if *ok { "parses" } else { "REJECTED" }
        );
    }

    assert!(
        holders.len() > 1,
        "expected {member} in more than one archive; duplicate parity is the control"
    );
    assert!(
        holders.iter().all(|(_, ok)| *ok),
        "every authored copy must parse: {holders:?}"
    );

    // Exercise the shared lookup used by `EntityModels` and `rigcompare`, not a test-only archive
    // scan. There should be no rejected copy once the lighting controller is understood.
    let lookup = caer_assets::figures::first_parseable_skeleton(&archives, &member);
    assert!(
        lookup.skeleton.is_some(),
        "the product lookup did not resolve the authored Firbolg skeleton"
    );
    assert!(
        lookup.rejected_archives.is_empty(),
        "a valid duplicate was rejected: {:?}",
        lookup.rejected_archives
    );

    // The duplicate sfig files also carry authoring-only attachment bones.  Those are allowed to
    // differ between the lit and stripped export, but they must not be used as evidence that the
    // body rig changed.  Compare the bones that the actual Firbolg base-body skins reference,
    // rather than every incidental bone in the archive.
    let figures =
        caer_assets::figures::FigureModels::load(root.join("gamedata.mpk")).expect("fig3");
    let body_bones: BTreeSet<String> = figures
        .base_body(10, 1)
        .into_iter()
        .filter_map(|part| {
            let bytes =
                caer_assets::open_member(root.join(part.archive_path()), &part.nif_member())
                    .ok()??;
            Some(caer_assets::nif::skin_bone_names(&bytes))
        })
        .flatten()
        .map(|name| name.to_ascii_lowercase())
        .collect();
    assert!(
        body_bones.len() > 50,
        "Firbolg base skins exposed too few bones: {}",
        body_bones.len()
    );

    let reference = caer_assets::open_member(&holders[0].0, &member)
        .expect("first Firbolg archive opens")
        .expect("first Firbolg member exists");
    let expected = caer_assets::nif::read_skeleton(&reference).expect("first Firbolg copy parses");
    for (archive, _) in holders.iter().skip(1) {
        let bytes = caer_assets::open_member(archive, &member)
            .expect("duplicate Firbolg archive opens")
            .expect("duplicate Firbolg member exists");
        let actual =
            caer_assets::nif::read_skeleton(&bytes).expect("duplicate Firbolg copy parses");
        assert_eq!(
            actual.bones.len(),
            expected.bones.len(),
            "bone count differs in {}",
            archive.display()
        );
        let mut compared = 0usize;
        for e in expected
            .bones
            .iter()
            .filter(|bone| body_bones.contains(&bone.name.to_ascii_lowercase()))
        {
            let a = actual
                .bones
                .iter()
                .find(|bone| bone.name == e.name)
                .unwrap_or_else(|| panic!("body bone {} missing in {}", e.name, archive.display()));
            for k in 0..9 {
                assert!(
                    (a.local.0[k] - e.local.0[k]).abs() < 1e-5,
                    "local rotation differs for {} in {}",
                    a.name,
                    archive.display()
                );
            }
            for k in 0..3 {
                assert!(
                    (a.local.2[k] - e.local.2[k]).abs() < 1e-5,
                    "local translation differs for {} in {}",
                    a.name,
                    archive.display()
                );
            }
            compared += 1;
        }
        assert!(
            compared > 50,
            "too few body bones compared in {}: {compared}",
            archive.display()
        );
    }
}

#[test]
fn firbolg_part_bones_versus_its_race_skeleton() {
    let root = root();
    let figures =
        caer_assets::figures::FigureModels::load(root.join("gamedata.mpk")).expect("fig3");

    for (race, label) in [(10u8, "Firbolg"), (1, "Briton (control)")] {
        let Some(member) = caer_assets::figures::FigureModels::skeleton_member(race, 1) else {
            continue;
        };
        // Find the skeleton in whichever sfig archive carries it.
        let mut skel_bones: BTreeSet<String> = BTreeSet::new();
        if let Ok(rd) = std::fs::read_dir(root.join("figures/fig3")) {
            let mut archives: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.to_ascii_lowercase().starts_with("sfig"))
                })
                .collect();
            archives.sort();
            // Stop on a parse, not on a name. The same member ships in more than one archive and
            // the copies are not equally readable — `sfig001`'s `Fir_*_skeleton.nif` carries a
            // leftover Max lighting rig while `sfig002` has a clean 129-bone copy — so breaking on
            // presence reported an empty bone set for Firbolg, the one race this test is named
            // after, and blamed a missing skeleton. `EntityModels::race_skeleton` keeps scanning.
            for a in archives {
                let Ok(Some(m)) = caer_assets::open_member(&a, &member) else {
                    continue;
                };
                if let Ok(sk) = caer_assets::nif::read_skeleton(&m) {
                    skel_bones = sk
                        .bones
                        .iter()
                        .map(|b| b.name.to_ascii_lowercase())
                        .collect();
                    break;
                }
            }
        }
        println!(
            "\n=== {label}: skeleton {member} has {} bones ===",
            skel_bones.len()
        );
        if skel_bones.is_empty() {
            println!("  skeleton not found — that alone would explain the mismatch");
            continue;
        }
        for part in figures.base_body(race, 1) {
            let Ok(Some(bytes)) =
                caer_assets::open_member(root.join(part.archive_path()), &part.nif_member())
            else {
                continue;
            };
            let Ok(Some(rig)) = caer_assets::nif::read_rigged(&bytes) else {
                continue;
            };
            let names: BTreeSet<String> = rig
                .skeleton
                .bones
                .iter()
                .map(|b| b.name.to_ascii_lowercase())
                .collect();
            let known = names.intersection(&skel_bones).count();
            let missing: Vec<&String> = names.difference(&skel_bones).take(4).collect();
            println!(
                "  {:<22} {:>3} bones, {:>3} in skeleton{}",
                part.filename,
                names.len(),
                known,
                if missing.is_empty() {
                    String::new()
                } else {
                    format!("  MISSING e.g. {missing:?}")
                }
            );
        }
    }
}

/// Every skeleton the client actually ships, so a missing one is a lookup bug not a data gap.
#[test]
fn list_shipped_skeletons() {
    let root = root();
    let mut all: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root.join("figures/fig3")) {
        let mut archives: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.to_ascii_lowercase().starts_with("sfig"))
            })
            .collect();
        archives.sort();
        for a in &archives {
            if let Ok(names) = caer_assets::list_names(a) {
                for n in names {
                    if n.to_ascii_lowercase().contains("skeleton") {
                        all.push(n);
                    }
                }
            }
        }
    }
    all.sort();
    println!("\n{} skeleton members shipped:", all.len());
    for n in &all {
        println!("  {n}");
    }
}

/// Both Firbolg skeleton copies must remain readable, including the one with Max lighting blocks.
#[test]
fn firbolg_skeleton_read_error() {
    let root = root();
    for member in ["Fir_m_skeleton.nif", "Bri_m_skeleton.nif"] {
        let mut found = false;
        if let Ok(rd) = std::fs::read_dir(root.join("figures/fig3")) {
            let mut archives: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.to_ascii_lowercase().starts_with("sfig"))
                })
                .collect();
            archives.sort();
            for a in &archives {
                if let Ok(Some(bytes)) = caer_assets::open_member(a, member) {
                    found = true;
                    println!(
                        "\n{member} in {}: {} bytes",
                        a.file_name().unwrap().to_string_lossy(),
                        bytes.len()
                    );
                    match caer_assets::nif::read_skeleton(&bytes) {
                        Ok(sk) => println!("  read_skeleton OK — {} bones", sk.bones.len()),
                        Err(e) => println!("  read_skeleton FAILED: {e}"),
                    }
                    break;
                }
            }
        }
        if !found {
            println!("\n{member}: not present in any sfig archive");
        }
    }
}

/// **Do the bound textures actually COVER their mesh part, or are they see-through?**
///
/// The matrix reports "0 bare" for a Briton with no arms. Binding a texture and drawing a visible
/// part are different facts, and every instrument so far measured the first.
///
/// The suspicion this tests: the starting set is CLOTHING, and clothing art is authored with
/// transparent regions where bare skin is meant to show through. Bound to a bare-skin mesh with no
/// skin layer underneath, a short sleeve becomes a missing forearm — which is exactly the Briton.
///
/// Reports each bound texture's opaque fraction. A body part whose texture is largely transparent
/// is a part the player will see holes in.
#[test]
fn bound_texture_translucency_census_does_not_grow() {
    let root = root();
    // SAFETY: single-threaded test entry; `EntityModels::load` reads the client root from env.
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 60_000.0)).expect("gpu init");
    let mut models = EntityModels::load().expect("entity models");

    // Index every Mskins member once so a texture can be pulled back by name.
    let mut index: std::collections::HashMap<String, PathBuf> = std::collections::HashMap::new();
    if let Ok(rd) = std::fs::read_dir(root.join("figures/Mskins")) {
        for e in rd.flatten() {
            if let Ok(names) = caer_assets::list_names(e.path()) {
                for n in names {
                    index.insert(n.to_ascii_lowercase(), e.path());
                }
            }
        }
    }

    let opaque_fraction = |name: &str| -> Option<f64> {
        let key = name.to_ascii_lowercase();
        let archive = index.get(&key)?;
        let bytes = caer_assets::open_member(archive, &key).ok()??;
        let tex = caer_assets::dds::read_model_dds(&bytes).ok()?;
        let (_, _, rgba) = tex.rgba8_mip0()?;
        let total = rgba.len() / 4;
        if total == 0 {
            return None;
        }
        let opaque = rgba.chunks_exact(4).filter(|p| p[3] > 200).count();
        Some(opaque as f64 / total as f64)
    };

    println!(
        "\n{:<12} {:<7} {:<26} {:>9}",
        "race", "gender", "texture", "opaque"
    );
    let mut holey: Vec<String> = Vec::new();
    for (race, label, wire) in legal_combinations() {
        let gender = caer_protocol::overview::fig3_gender_from_db(wire);
        if models
            .ensure_avatar(
                &mut gpu,
                race,
                gender,
                caer_protocol::customization::Customization::default(),
                None,
            )
            .is_none()
        {
            continue;
        }
        let g = if wire == 0 { "male" } else { "female" };
        for (part, tex) in &models.last_avatar_stand().bound_textures {
            if tex == "<none>" {
                continue;
            }
            // An unresolvable name is the more dangerous case and used to be skipped silently:
            // the binder records a texture the GPU never receives, so the part draws untextured
            // or not at all while every counter reports it bound.
            let Some(frac) = opaque_fraction(tex) else {
                println!("{label:<12} {g:<7} {tex:<26}    UNRESOLVED  <- {part}");
                holey.push(format!("{label} {g} {part} ({tex}) UNRESOLVED"));
                continue;
            };
            if frac < 0.85 {
                println!(
                    "{label:<12} {g:<7} {tex:<26} {:>8.1}%  <- {part}",
                    frac * 100.0
                );
                holey.push(format!("{label} {g} {part} ({tex}) {:.0}%", frac * 100.0));
            }
        }
    }
    if holey.is_empty() {
        println!("\nevery bound texture is >=85% opaque");
    } else {
        println!(
            "\n{} part(s) wear a texture below 85% opaque. Transparency alone is NOT a visible \
             hole: ledger A3/A4 falsified the alpha-cutout explanation for the Avalonian and \
             Saracen gloves, whose sheets upload at 100% alpha>=0.4 and still showed holes.",
            holey.len()
        );
    }

    // CENSUS WITH A RATCHET, not a behaviour gate. 221 parts are below the threshold today and no
    // policy says what the right number is, so asserting zero would be a guessed threshold. What
    // this can honestly assert is that the population does not GROW: a change that makes more
    // sheets translucent fails here, and a repair that shrinks the count fails too so the pin gets
    // tightened rather than left stale.
    const TRANSLUCENT_BASELINE: usize = 221;
    assert_eq!(
        holey.len(),
        TRANSLUCENT_BASELINE,
        "translucent bound-texture count moved from {TRANSLUCENT_BASELINE} to {} — if this grew, \
         a binding regressed; if it shrank, a repair landed and the baseline should come down",
        holey.len()
    );
}

/// **Where each posed part actually ENDS UP.**
///
/// The instrument gap Matt's screenshots exposed: every metric so far measured whether a part
/// bound a texture, never whether it lands anywhere a player can see. A Briton reports "0 bare"
/// and has no arms; a Highlander reports 8 of 8 textured and has no head.
///
/// This skins each part on the CPU with the same palette the shader uses and reports its posed
/// bounding box against the body's. Three failures become distinguishable and each names itself:
/// a part that collapses to a point, a part that flies clear of the body, and a part that sits
/// where it should.
#[test]
fn posed_part_bounds_stay_on_the_body() {
    let root = root();
    // SAFETY: single-threaded test entry; `EntityModels::load` reads the client root from env.
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 60_000.0)).expect("gpu init");
    let mut models = EntityModels::load().expect("entity models");

    let mut suspicious: Vec<String> = Vec::new();
    for (race, label, wire) in [(1u8, "Briton", 0u8), (3, "Highlander", 0), (6, "Troll", 0)] {
        let gender = caer_protocol::overview::fig3_gender_from_db(wire);
        let Some(model) = models.ensure_avatar(
            &mut gpu,
            race,
            gender,
            caer_protocol::customization::Customization::default(),
            None,
        ) else {
            continue;
        };
        let Some(rig) = models.skinned_rig(model) else {
            continue;
        };
        let palettes = caer_render::anim_skin::build_palettes(
            rig,
            &[caer_render::anim_skin::UniquePaletteJob {
                loco: caer_render::entities::Loco::Idle,
                t: 0.0,
                blend: None,
            }],
        );
        let batch = caer_render::terrain::skinned_batch_multi(&rig.rigs);
        println!(
            "\n=== {label} — {} parts, {} palette mats ===",
            batch.parts.len(),
            palettes.len()
        );
        println!(
            "  {:<22} {:>7} {:>26} {:>26}",
            "part", "verts", "min xyz", "max xyz"
        );

        for part in &batch.parts {
            let (s, e) = (part.start as usize, part.end as usize);
            let mut lo = [f32::MAX; 3];
            let mut hi = [f32::MIN; 3];
            let mut n = 0usize;
            for &vi in batch.indices.get(s..e).unwrap_or(&[]) {
                let Some(v) = batch.vertices.get(vi as usize) else {
                    continue;
                };
                let Some(sk) = batch.skin.get(vi as usize) else {
                    continue;
                };
                // Same blend the vertex shader performs: sum(weight * palette[joint] * pos).
                let mut p = [0.0f32; 3];
                for k in 0..4 {
                    let w = sk.weights[k];
                    if w <= 0.0 {
                        continue;
                    }
                    let Some(m) = palettes.get(sk.joints[k] as usize) else {
                        continue;
                    };
                    for axis in 0..3 {
                        p[axis] += w
                            * (m[0][axis] * v.pos[0]
                                + m[1][axis] * v.pos[1]
                                + m[2][axis] * v.pos[2]
                                + m[3][axis]);
                    }
                }
                if !p.iter().all(|c| c.is_finite()) {
                    continue;
                }
                for axis in 0..3 {
                    lo[axis] = lo[axis].min(p[axis]);
                    hi[axis] = hi[axis].max(p[axis]);
                }
                n += 1;
            }
            if n == 0 {
                println!("  {:<22} {:>7}  NO POSED VERTICES", part.name, 0);
                suspicious.push(format!("{label}/{} has no posed vertices", part.name));
                continue;
            }
            let extent = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
            let biggest = extent[0].max(extent[1]).max(extent[2]);
            println!(
                "  {:<22} {n:>7} {:>8.0},{:>8.0},{:>8.0} {:>8.0},{:>8.0},{:>8.0}",
                part.name, lo[0], lo[1], lo[2], hi[0], hi[1], hi[2]
            );
            // A part that collapses has no extent; a part that escapes sits far off the body.
            if biggest < 0.5 {
                suspicious.push(format!("{label}/{} COLLAPSED to a point", part.name));
            } else if lo.iter().chain(hi.iter()).any(|c| c.abs() > 500.0) {
                suspicious.push(format!(
                    "{label}/{} is {biggest:.0} units across, far off-body",
                    part.name
                ));
            }
        }
    }

    println!("\nsuspicious parts:");
    // Asserted, not printed. Every part currently poses onto the body, so this is a real gate:
    // a part that collapses to a point or flies clear of the figure fails here rather than
    // scrolling past in the log.
    assert!(
        suspicious.is_empty(),
        "{} part(s) do not pose onto the body: {:?}",
        suspicious.len(),
        suspicious
    );
    if suspicious.is_empty() {
        println!(
            "  none — every part poses onto the body, so a missing part is a DRAW issue, \
                  not a posing one"
        );
    } else {
        for s in &suspicious {
            println!("  {s}");
        }
    }
}

/// What skin art exists for a race's bare limbs, as opposed to its clothing.
#[test]
fn bare_limb_skin_candidates() {
    let root = root();
    let members = caer_assets::open(root.join("gamedata.mpk")).expect("gamedata");
    let txt = |n: &str| {
        members
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(n))
            .map(|m| String::from_utf8_lossy(&m.data).into_owned())
            .unwrap_or_default()
    };
    for (table, csv) in [("mskins", txt("mskins.csv")), ("skins", txt("skins.csv"))] {
        let mut hits: Vec<String> = Vec::new();
        for line in csv.lines() {
            let c: Vec<&str> = line.split(',').map(str::trim).collect();
            if c.len() < 3 || c[0].parse::<u32>().is_err() {
                continue;
            }
            let n = c[2].to_ascii_lowercase();
            if n.starts_with("bri_m") && !n.contains("hair") && !n.contains("head") {
                hits.push(format!("{}={}", c[0], c[2]));
            }
        }
        println!("\n{table}: {} bri_m non-head/hair rows", hits.len());
        for h in hits.iter().take(14) {
            println!("  {h}");
        }
    }
}

/// Which starting-set candidate names actually exist in `figures/Mskins`.
///
/// The binder tries `<slot>_SS_<tok>_<G>.dds` then `<slot>_SS_<tok>.dds`. If the first does not
/// exist the second must, or the part goes untextured — and a report that prints the name it
/// *asked for* rather than the one it *found* hides that completely.
#[test]
fn starting_set_candidate_names_resolve() {
    let root = root();
    let mut index: BTreeSet<String> = BTreeSet::new();
    if let Ok(rd) = std::fs::read_dir(root.join("figures/Mskins")) {
        for e in rd.flatten() {
            if let Ok(names) = caer_assets::list_names(e.path()) {
                index.extend(names.into_iter().map(|n| n.to_ascii_lowercase()));
            }
        }
    }
    println!("\n{} Mskins members indexed", index.len());
    let slots = [
        "cthBody01",
        "cthArms01",
        "cthGloves01",
        "cthLegs01",
        "cthBoots01",
    ];
    println!(
        "\n{:<10} {:<7} {:<26} {:<26}",
        "race", "gender", "gendered name", "fallback"
    );
    for race in [1u8, 3, 6, 9] {
        let Some(tok) = caer_assets::figures::FigureModels::race_token(race) else {
            continue;
        };
        for (g, gn) in [('M', "male"), ('F', "female")] {
            for slot in slots {
                let a = format!("{slot}_SS_{tok}_{g}.dds").to_ascii_lowercase();
                let b = format!("{slot}_SS_{tok}.dds").to_ascii_lowercase();
                let (ha, hb) = (index.contains(&a), index.contains(&b));
                if !ha && !hb {
                    println!("{tok:<10} {gn:<7} {a:<26} {b:<26}  NEITHER EXISTS");
                }
            }
        }
    }
    println!("(rows printed above are slots with no starting-set texture at all)");
}

/// Is there a texture named after the bare-limb MESH, the way the head has one?
#[test]
fn bare_limb_mesh_named_textures() {
    let root = root();
    let mut index: BTreeSet<String> = BTreeSet::new();
    if let Ok(rd) = std::fs::read_dir(root.join("figures/Mskins")) {
        for e in rd.flatten() {
            if let Ok(names) = caer_assets::list_names(e.path()) {
                index.extend(names.into_iter().map(|n| n.to_ascii_lowercase()));
            }
        }
    }
    let figures =
        caer_assets::figures::FigureModels::load(root.join("gamedata.mpk")).expect("fig3");
    println!("\n{:<22} {:>10}", "mesh part", "own .dds?");
    for part in figures.base_body(1, 1) {
        let want = format!("{}.dds", part.filename.to_ascii_lowercase());
        println!(
            "{:<22} {:>10}",
            part.filename,
            if index.contains(&want) { "YES" } else { "-" }
        );
    }
    // Anything at all matching the bare-limb stems.
    for stem in ["arms01", "gloves01", "body01"] {
        let hits: Vec<&String> = index
            .iter()
            .filter(|n| n.starts_with(stem))
            .take(8)
            .collect();
        println!("  Mskins entries starting `{stem}`: {hits:?}");
    }
}

/// **Is the head texture a single face, or an atlas of many?**
///
/// The claim in `entities.rs` is that `<race>_<g>_head01.dds` is an ATLAS holding several faces and
/// that the character's is one sub-region chosen via `fig3facemap.csv` — which is why heads render
/// untextured today. That is load-bearing for every "no face" report, and it was written from
/// reasoning rather than from the image.
///
/// This measures the sheet: an atlas of N faces has N clusters of skin-toned pixels separated by
/// gaps, while a single face fills its sheet. Whatever it shows decides whether the fix is a UV
/// selection or a plain bind.
#[test]
fn head_texture_is_an_atlas_or_a_single_face() {
    let root = root();
    let mut index: std::collections::HashMap<String, PathBuf> = std::collections::HashMap::new();
    if let Ok(rd) = std::fs::read_dir(root.join("figures/Mskins")) {
        for e in rd.flatten() {
            if let Ok(names) = caer_assets::list_names(e.path()) {
                for n in names {
                    index.insert(n.to_ascii_lowercase(), e.path());
                }
            }
        }
    }

    println!(
        "\n{:<26} {:>10} {:>9} {:>26}",
        "head texture", "size", "opaque", "row-occupancy profile"
    );
    for name in [
        "bri_m_head01.dds",
        "bri_f_head01.dds",
        "hig_m_head01.dds",
        "tro_m_head01.dds",
    ] {
        let Some(archive) = index.get(name) else {
            println!("{name:<26}  NOT IN Mskins");
            continue;
        };
        let Ok(Some(bytes)) = caer_assets::open_member(archive, name) else {
            continue;
        };
        let Some(tex) = caer_assets::dds::read_model_dds(&bytes).ok() else {
            continue;
        };
        let Some((w, h, rgba)) = tex.rgba8_mip0() else {
            continue;
        };
        let opaque = rgba.chunks_exact(4).filter(|p| p[3] > 16).count();
        // Occupancy per horizontal band: an atlas shows filled/empty banding, one face does not.
        let bands = 8;
        let mut profile = String::new();
        for b in 0..bands {
            let y0 = h as usize * b / bands;
            let y1 = h as usize * (b + 1) / bands;
            let mut lit = 0usize;
            let mut total = 0usize;
            for y in y0..y1 {
                for x in 0..w as usize {
                    let i = (y * w as usize + x) * 4;
                    if let Some(px) = rgba.get(i..i + 4) {
                        total += 1;
                        if px[3] > 16 && (px[0] as u16 + px[1] as u16 + px[2] as u16) > 90 {
                            lit += 1;
                        }
                    }
                }
            }
            let frac = lit as f64 / total.max(1) as f64;
            profile.push(match (frac * 10.0) as u32 {
                0 => '.',
                1..=3 => '-',
                4..=6 => '+',
                _ => '#',
            });
        }
        println!(
            "{name:<26} {:>4}x{:<5} {:>8.1}% {profile:>26}",
            w,
            h,
            opaque as f64 * 100.0 / (rgba.len() / 4).max(1) as f64
        );
    }
    println!("  '#' = dense, '.' = empty. Banding implies an atlas; uniform implies one face.");
}

/// **Which skeleton each body binds against, and how well.**
///
/// The authored Firbolg skeleton contains a light-colour controller in one archive. If that block
/// desynchronizes the NIF reader, the renderer silently falls back to `common_body_skeleton` and
/// its ~66-unit drift. This reports the actual parse state so that fallback cannot hide again.
///
/// This reports, per race and gender, whether the authored skeleton exists, whether it parses, and
/// its bone count. A race falling back is the defect; the bind error is only the symptom.
#[test]
fn every_body_binds_its_own_authored_skeleton() {
    let root = root();
    let mut archives: Vec<PathBuf> = std::fs::read_dir(root.join("figures/fig3"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.to_ascii_lowercase().starts_with("sfig"))
        })
        .collect();
    archives.sort();

    println!(
        "\n{:<12} {:<7} {:<26} {:>7} {:>7}",
        "race", "gender", "skeleton", "bones", "state"
    );
    let mut fallback: Vec<String> = Vec::new();
    for (race, label, wire) in legal_combinations() {
        let gender = caer_protocol::overview::fig3_gender_from_db(wire);
        let Some(member) = caer_assets::figures::FigureModels::skeleton_member(race, gender) else {
            continue;
        };
        // Search every archive, exactly as `EntityModels::race_skeleton` does. Duplicate members
        // are allowed, and every parseable copy is a valid candidate; the first parseable one wins.
        let mut state = "MISSING";
        let mut bones = 0usize;
        for a in &archives {
            let Ok(Some(bytes)) = caer_assets::open_member(a, &member) else {
                continue;
            };
            match caer_assets::nif::read_skeleton(&bytes) {
                Ok(sk) => {
                    bones = sk.bones.len();
                    state = "ok";
                    break;
                }
                Err(_) => state = "UNREADABLE",
            }
        }
        let g = if wire == 0 { "male" } else { "female" };
        println!("{label:<12} {g:<7} {member:<26} {bones:>7} {state:>7}");
        if state != "ok" {
            fallback.push(format!("{label} {g} ({member}: {state})"));
        }
    }

    println!("\nbodies with no usable authored skeleton — these fall back and fail the bind gate:");
    if fallback.is_empty() {
        println!("  none");
    } else {
        for f in &fallback {
            println!("  {f}");
        }
    }

    // One documented class, pinned so a second cannot appear silently: the three Minotaur races
    // ship no `mino_m_skeleton.nif` because they do not use the fig3 path (ledger B7).
    let unexpected: Vec<&String> = fallback
        .iter()
        .filter(|f| !f.starts_with("Minotaur"))
        .collect();
    assert!(
        unexpected.is_empty(),
        "a body outside the fig3-exempt races has no usable authored skeleton: {unexpected:?}"
    );
    assert_eq!(
        fallback.len(),
        3,
        "expected exactly the three fig3-exempt Minotaur races, got {fallback:?}"
    );
}

/// **Where each part's triangles land on its own sheet — and why that is not the whole answer.**
///
/// UV min/max bounds could never say where polygons sit, so this rasterises each triangle onto its
/// bound sheet and reports how much lands on the filler packed around the islands.
///
/// **Read the ranking with the counter-example below in mind.** A high number here does *not* mean
/// a visible defect. `CthGloves01_SS_Dwa.dds` packs the space between its islands with the glove's
/// own leather, so two thirds of the Dwarf's glove area samples "filler" and the glove renders
/// perfectly; the Highlander's boots sample a third as much and show a grey wedge, because that
/// sheet's filler is a flat neutral grey. Filler that matches the material is invisible.
///
/// Whether a player can see it is answered by `CAER_FILLER_PROBE=1`, which repaints filler magenta
/// at upload so a capture shows exactly which pixels sampled it, and `caer audit filler`, which
/// **Ledger A5.** Every cloth sheet we bind is one the client's own index lists, except for the
/// one measured retail fallback: Highlander male legs use the unisex `cthLegs01_SS_hig.dds` art
/// even though the shipped CSV indexes only its gendered duplicate.
///
/// `assign_mskin_cloth` builds a candidate file name (`cth<Slot>01_SS_<Token>_<M|F>.dds`) and takes
/// the first that resolves in any `figures/Mskins/*.mpk`. "A file with this name exists" and "the
/// client ships this file for this race and gender" are different questions, and **15 bindings
/// answered them differently** — every one male, including **Briton male legs**, half the pair in
/// the side-by-side that reopened A5. `cthLegs01_ss_Bri_m.dds` is in `mskin014` and is not in
/// `mskins.csv`; the client dresses Briton male legs in the unisex `cthLegs01_ss_Bri.dds`.
///
/// Measured effect: the Briton female/male legs luma ratio moved **1.11 → 0.95** on this fix alone
/// (Eden renders 0.48, so this is a step, not the finish).
///
/// Seen red by removing the `tbl.indexed` filter from the binder: 15 parts bind unindexed art.
#[test]
fn every_bound_cloth_sheet_is_one_the_client_indexes() {
    use caer_render::uv_coverage;

    let root = root();
    // SAFETY: single-threaded test entry; `EntityModels::load` reads the client root from env.
    std::env::set_var("CAER_CLIENT", &root);
    let mskins = caer_assets::mskins::Mskins::load(root.join("gamedata.mpk"))
        .expect("gamedata.mpk : mskins.csv");
    assert!(
        mskins.len() > 2000,
        "the index parsed {} rows — too few to be the real table, so a pass below would be vacuous",
        mskins.len()
    );

    let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 60_000.0)).expect("gpu init");
    let mut models = EntityModels::load().expect("entity models");

    let mut unindexed: Vec<String> = Vec::new();
    let mut checked = 0usize;
    for (race, label, wire) in legal_combinations() {
        let gender = caer_protocol::overview::fig3_gender_from_db(wire);
        let g = if wire == 0 { "male" } else { "female" };
        for pc in uv_coverage::avatar_coverage(&mut models, &mut gpu, race, gender) {
            // The binder records the key with its `#opaque` suffix; the index knows file names.
            let file = pc.texture.split('#').next().unwrap_or(&pc.texture);
            let low = file.to_ascii_lowercase();
            // Only the starting-set cloth is table-driven. Heads and hair resolve by mesh name.
            if !(low.starts_with("cth") || low.starts_with("scl")) {
                continue;
            }
            checked += 1;
            // The Highlander starter outfit is the same kind of authored unisex fallback already
            // pinned for Briton legs: the retail tree contains the correct sheet, while mskins.csv
            // indexes only the pale `_M` duplicate.  The renderer deliberately selects the unisex
            // sheet to match the reference capture, so keep that exception explicit in the gate.
            let approved_unisex_fallback = race == 3 && wire == 0 && low == "cthlegs01_ss_hig.dds";
            if !mskins.indexed(file) && !approved_unisex_fallback {
                unindexed.push(format!("{label} {g} {} -> {file}", pc.part));
            }
        }
    }
    assert!(
        checked > 100,
        "only {checked} cloth bindings inspected — the roster did not load, so this proves nothing"
    );
    assert!(
        unindexed.is_empty(),
        "{} cloth binding(s) use art the client does not index:\n  {}",
        unindexed.len(),
        unindexed.join("\n  ")
    );
}

/// **Ledger A5.** What every cloth part actually samples out of its sheet — alpha and colour, over
/// the geometry rather than over the page.
///
/// Whole-page statistics are what made me close A5 wrongly. `cthLegs01_ss_Bri_f.dds` is 37% dark
/// low-alpha texels and 63% light ones *across the whole atlas*, but a leg samples only its own
/// island, and a prediction built on the page average did not reproduce Eden's render. This is the
/// same question asked of the triangles.
///
/// Prints rather than asserts thresholds, except for the one structural claim worth pinning: **a
/// bound sheet that never reaches opaque under its own geometry is an overlay**, and the binder
/// force-opaques it anyway. The count is asserted so it cannot silently drop to zero (which would
/// make this look clean) or quietly grow.
#[test]
fn cloth_sheets_report_what_their_geometry_samples() {
    use caer_render::uv_coverage;

    let root = root();
    // SAFETY: single-threaded test entry; `EntityModels::load` reads the client root from env.
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 60_000.0)).expect("gpu init");
    let mut models = EntityModels::load().expect("entity models");

    println!(
        "\n{:<14} {:<7} {:<10} {:<34} {:>7} {:>7} {:>7} {:>8}",
        "race", "gender", "part", "sheet", "aMax", "aMean", "luma", "verdict"
    );
    let mut overlays = 0usize;
    // Per race: the female-to-male luma ratio of the SAME slot. This is the confound-free shape of
    // the screenshot measurement (Eden 0.48, CAER 1.66) computed on the source art instead.
    let mut by_slot: std::collections::HashMap<(String, String), [Option<f32>; 2]> =
        std::collections::HashMap::new();

    for (race, label, wire) in legal_combinations() {
        let gender = caer_protocol::overview::fig3_gender_from_db(wire);
        let g = if wire == 0 { "male" } else { "female" };
        for pc in uv_coverage::avatar_coverage(&mut models, &mut gpu, race, gender) {
            let Some((w, h)) = pc.sheet else { continue };
            let s = pc.sampled;
            if s.texels == 0 {
                continue;
            }
            let overlay = s.reads_as_overlay();
            if overlay {
                overlays += 1;
            }
            let slot: String = pc
                .part
                .to_ascii_lowercase()
                .trim_end_matches(|c: char| {
                    c.is_ascii_digit() || c == '_' || c.is_alphabetic() && false
                })
                .to_string();
            let slot = slot
                .split(|c: char| c.is_ascii_digit())
                .next()
                .unwrap_or(&slot)
                .to_string();
            by_slot
                .entry((label.to_string(), slot))
                .or_insert([None, None])[usize::from(wire != 0)] = Some(s.luma_linear());
            if overlay {
                println!(
                    "{label:<14} {g:<7} {:<10} {:<34} {:>7} {:>7.1} {:>7.1} {:>8}",
                    pc.part,
                    format!("{} {w}x{h}", pc.texture),
                    s.alpha_max,
                    s.alpha_mean,
                    s.luma(),
                    "OVERLAY",
                );
            }
        }
    }

    println!(
        "\n--- female/male LINEAR luma ratio per race+slot (encoded means are not ratio-able; \
         Eden's Briton legs measure 0.48 in a screenshot, i.e. encoded) ---"
    );
    let mut rows: Vec<(String, String, f32)> = by_slot
        .into_iter()
        .filter_map(|((race, slot), [m, f])| Some((race, slot, f? / m?)))
        .collect();
    rows.sort_by(|a, b| a.2.total_cmp(&b.2));
    for (race, slot, ratio) in &rows {
        println!("  {race:<14} {slot:<10} {ratio:>6.2}");
    }

    assert!(
        overlays > 0,
        "no sampled sheet reads as an overlay — either the client changed or `sample_texels` \
         measured nothing, and both are findings rather than a clean bill of health"
    );
    println!("\n{overlays} bound sheet(s) never reach opaque under their own geometry");
}

/// separates a solid patch from a texel of island bleed. This test is the map; that is the camera.
#[test]
fn part_uvs_land_on_art_not_on_atlas_filler() {
    use caer_render::uv_coverage;

    let root = root();
    // SAFETY: single-threaded test entry; `EntityModels::load` reads the client root from env.
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 60_000.0)).expect("gpu init");
    let mut models = EntityModels::load().expect("entity models");

    println!(
        "\n{:<14} {:<7} {:<20} {:<30} {:>9} {:>8} {:>8} {:>7}",
        "race", "gender", "part", "sheet", "sheet_bg", "tri_bg", "area_bg", "wrapped"
    );
    // The filler COLOUR is printed with every row on purpose. A flat region of art — a complexion
    // on a face sheet, leather packed between glove islands — looks exactly like packing waste to
    // any measure of flatness, and the colour is what tells them apart at a glance.

    let mut worst: Vec<(f32, String)> = Vec::new();
    let mut unresolved: Vec<String> = Vec::new();
    let mut probe: std::collections::HashMap<(&str, &str), f32> = std::collections::HashMap::new();

    for (race, label, wire) in legal_combinations() {
        let gender = caer_protocol::overview::fig3_gender_from_db(wire);
        let g = if wire == 0 { "male" } else { "female" };
        for pc in uv_coverage::avatar_coverage(&mut models, &mut gpu, race, gender) {
            let sheet = pc
                .sheet
                .map_or_else(|| "UNRESOLVED".to_string(), |(w, h)| format!("{w}x{h}"));
            if pc.sheet.is_none() {
                unresolved.push(format!("{label} {g} {} ({})", pc.part, pc.texture));
            }
            let area = pc.coverage.filler_area_fraction();
            if pc.background.is_some() && (area > 0.02 || pc.coverage.triangles_wrapped > 0) {
                println!(
                    "{label:<14} {g:<7} {:<20} {:<30} {:>8.1}% {:>7.1}% {:>7.1}% {:>7}  bg={:?}",
                    pc.part,
                    format!("{} {sheet}", pc.texture),
                    pc.background_fraction * 100.0,
                    pc.coverage.filler_triangle_fraction() * 100.0,
                    area * 100.0,
                    pc.coverage.triangles_wrapped,
                    pc.background,
                );
            }
            if pc.measured() && area > 0.05 {
                worst.push((area, format!("{label} {g} {} ({})", pc.part, pc.texture)));
            }
            let p = pc.part.to_ascii_lowercase();
            if wire == 0 {
                if race == 3 && p.starts_with("boots") {
                    probe.insert(("Highlander", "boots"), area);
                } else if race == 3 && p.starts_with("lbody") {
                    probe.insert(("Highlander", "kilt"), area);
                } else if race == 7 && p.starts_with("gloves") {
                    probe.insert(("Dwarf", "gloves"), area);
                }
            }
        }
    }

    let boots = probe.get(&("Highlander", "boots")).copied();
    let kilt = probe.get(&("Highlander", "kilt")).copied();
    let dwarf = probe.get(&("Dwarf", "gloves")).copied();
    println!(
        "\ncalibration — Highlander Boots {boots:?}, Highlander kilt {kilt:?}, Dwarf gloves {dwarf:?}"
    );
    let (Some(boots), Some(kilt), Some(dwarf)) = (boots, kilt, dwarf) else {
        panic!("calibration parts not measured: boots={boots:?} kilt={kilt:?} dwarf={dwarf:?}");
    };

    // The kilt's sheet has no reachable filler and the kilt renders correctly, so it must read
    // clean. An instrument that reports it dirty is measuring its own noise.
    assert!(
        kilt < 0.02,
        "the kilt renders correctly and must read clean; got {:.1}%",
        kilt * 100.0
    );

    // **The counter-example, asserted so it cannot be quietly forgotten.** The Dwarf's gloves score
    // far higher than the Highlander's boots and are the ones that render correctly. If this ever
    // flips, either the sheets changed or this measure started meaning something it does not, and
    // either way the ranking above must not be read as a defect list.
    assert!(
        dwarf > boots,
        "Dwarf gloves ({:.1}%) are expected to out-score Highlander boots ({:.1}%) while looking \
         correct — that inversion is the documented reason UV filler area is not a defect ranking. \
         If it no longer holds, re-derive the limitation before trusting either number.",
        dwarf * 100.0,
        boots * 100.0
    );

    worst.sort_by(|a, b| b.0.total_cmp(&a.0));
    println!(
        "\n{} part(s) sample atlas filler over 5% of their UV area — a map of where to point \
         CAER_FILLER_PROBE, NOT a defect list:",
        worst.len()
    );
    for (f, what) in worst.iter().take(40) {
        println!("  {:>6.1}%  {what}", f * 100.0);
    }
    if !unresolved.is_empty() {
        println!(
            "\n{} bound texture(s) did not resolve to a sheet:",
            unresolved.len()
        );
        for u in unresolved.iter().take(20) {
            println!("  {u}");
        }
    }
}

/// **The bound-texture report only sees the fig3 path.**
///
/// `CAER_POSE_REPORT` lists what each part wears, and every "N/N textured" reading assumes the
/// report covers the avatar. It does not: it covers *fig3-assembled* avatars. A body that stands
/// through the living-model path contributes **zero rows**, so "no parts reported" and "no body
/// exists" are the same output.
///
/// That distinction is not academic. This test first concluded the three Minotaur races had no body
/// at all, because `ensure_avatar` — the fig3 entry point — returns `None` for them. Matt pointed
/// out that Deifrang, Korazh and Graoch all render correctly, and a capture confirms it: `path=living
/// parts=[living:1395] textured=1`, head texture and armour intact. The instrument was blind, not
/// the client. Ledger B7 survived this long for the same reason — the report that would have
/// contradicted it cannot see that path.
///
/// Pinned so the gap stays visible. If the living path ever reports rows here, this goes red and
/// the report can finally be calibrated for absence.
#[test]
fn the_bound_texture_report_only_covers_the_fig3_path() {
    let root = root();
    // SAFETY: single-threaded test entry; `EntityModels::load` reads the client root from env.
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 60_000.0)).expect("gpu init");
    let mut models = EntityModels::load().expect("entity models");

    let mut clean = 0usize;
    let mut holed: Vec<String> = Vec::new();
    let mut not_fig3: Vec<String> = Vec::new();

    for (race, label, wire) in legal_combinations() {
        let gender = caer_protocol::overview::fig3_gender_from_db(wire);
        let g = if wire == 0 { "m" } else { "f" };
        if models
            .ensure_avatar(
                &mut gpu,
                race,
                gender,
                caer_protocol::customization::Customization::default(),
                None,
            )
            .is_none()
        {
            not_fig3.push(format!("{label} {g} (race {race})"));
            continue;
        }
        let stand = models.last_avatar_stand();
        let total = stand.bound_textures.len();
        let none = stand
            .bound_textures
            .iter()
            .filter(|(_, tex)| tex == "<none>")
            .count();
        if total == 0 {
            not_fig3.push(format!("{label} {g} (race {race})"));
        } else if none > 0 {
            holed.push(format!("{label} {g}: {none}/{total} unbound"));
        } else {
            clean += 1;
        }
    }

    println!("\nbound-texture report over every legal body:");
    println!(
        "  {clean} fig3 bodies fully bound, {} with an unbound part, {} not on the fig3 path",
        holed.len(),
        not_fig3.len()
    );
    for a in &not_fig3 {
        println!(
            "    NOT FIG3  {a}  (renders via the living-model path; invisible to this report)"
        );
    }
    for h in &holed {
        println!("    UNBOUND   {h}");
    }

    assert!(clean > 0, "not one fig3 body reports every part bound");

    // The gap, asserted so it cannot quietly close without someone noticing.
    assert!(
        !not_fig3.is_empty(),
        "every legal race now assembles through fig3. If that is real, the living-model races were \
         ported and this characterisation is stale — replace it with a proper calibration."
    );
    assert!(
        holed.is_empty(),
        "a fig3 body now reports an unbound part ({holed:?}). That is the known-bad this reporter \
         has never produced — calibrate CAER_POSE_REPORT against it and delete this test."
    );
}

/// **The style override may refine a hairstyle; it may not change which skull the mesh fits.**
///
/// The override pairs a hair mesh with its colour sheet, because fig3's base-body hair part and
/// the colour table's style row are looked up separately and disagree for several races — Kobold
/// male gets a `kob_m_hair01` mesh with a `kob_m_hair02` sheet. Pairing those is right.
///
/// It derives the mesh name from the *sheet*, though, and the colour table hands some races
/// another race's sheet. Avalonian female resolves `nor_f_hair01_white.dds`, so the override
/// replaced her authored `ava_f_hair01.nif` with the Norseman cap — cut for a Norseman skull and
/// sitting high on hers. That is ledger A1.
///
/// The comparison is on `<race>_<gender>`, because both halves pick the skull.
///
/// **The authored mesh is the authority, not the body's race.** What must hold is narrow: whatever
/// fig3 assigns, the override may change the style number and not the race.
///
/// **Ledger E4/E4a — this test used to require the opposite and was green because of a defect.**
/// It asserted `cross_race_authored > 0`, on the belief that fig3map assigns Inconnu female a
/// `bri_f_hair02.nif` and Half Ogre male a `hig_m_hair03.nif`. It does not. Those two bindings were
/// produced by our own reader: 12 part ids in `fig3parts.csv` are claimed twice, the map took the
/// last claim, and ids 191-194 — `inc_f_Hair01..04` — resolved to `bri_f_Hair02`. Reading the
/// client's table through the broken parser and calling the result "the client's own choice" is
/// how a bug got written into a test as ground truth, and the assertion then held it in place.
///
/// So the count is inverted. **Zero** is the client's real answer, and a non-zero value now means
/// either the client data changed or part resolution regressed — which is E4 coming back.
#[test]
fn the_hair_style_override_never_changes_the_authored_mesh_race() {
    let root = root();
    // SAFETY: single-threaded test entry; `EntityModels::load` reads the client root from env.
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 60_000.0)).expect("gpu init");
    let mut models = EntityModels::load().expect("entity models");
    let figs = caer_assets::figures::FigureModels::load(root.join("gamedata.mpk"))
        .expect("gamedata.mpk must load — this test compares against fig3map");

    // `<race>_<gender>` — both halves. Gender is not a detail: Inconnu male authors
    // `inc_m_hair01.nif` and the override swapped it for `inc_f_hair01`, a female cap on a male
    // skull, which a race-only comparison waved through.
    let token = |name: &str| -> String {
        let lower = name.to_ascii_lowercase();
        let stem = lower
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .trim_end_matches(".nif")
            .trim_end_matches(".dds")
            .to_string();
        let mut it = stem.split('_');
        match (it.next(), it.next()) {
            (Some(r), Some(g)) if !r.is_empty() && !g.is_empty() => format!("{r}_{g}"),
            _ => stem,
        }
    };

    let mut checked = 0usize;
    let mut cross_race_authored = 0usize;
    let mut restyled = 0usize;
    let mut wrong: Vec<String> = Vec::new();

    for (race, label, wire) in legal_combinations() {
        let gender = caer_protocol::overview::fig3_gender_from_db(wire);
        let g = if wire == 0 { "m" } else { "f" };
        if models
            .ensure_avatar(
                &mut gpu,
                race,
                gender,
                caer_protocol::customization::Customization::default(),
                None,
            )
            .is_none()
        {
            continue;
        }
        // What fig3map authored for this body's hair slot.
        let Some(authored) = figs
            .base_body(race, gender)
            .into_iter()
            .find(|p| p.filename.to_ascii_lowercase().contains("hair"))
        else {
            continue;
        };
        let want = token(&authored.filename);
        if let Some(body) = caer_assets::figures::FigureModels::race_token(race) {
            if !want.starts_with(&format!("{}_", body.to_ascii_lowercase())) {
                cross_race_authored += 1;
            }
        }
        for (part, tex) in &models.last_avatar_stand().bound_textures {
            let p = part.to_ascii_lowercase();
            if !p.contains("hair") {
                continue;
            }
            checked += 1;
            // The SHEET may come from anywhere — it is colour, not shape.
            if token(tex) != want {
                restyled += 1;
            }
            if token(&p) != want {
                wrong.push(format!(
                    "{label} {g}: fig3 authored {} but {part} was bound",
                    authored.filename
                ));
            }
        }
    }

    println!(
        "hair: {checked} meshes checked, {cross_race_authored} authored cross-race by fig3 itself, \
         {restyled} wearing a sheet from another race (allowed)"
    );
    assert!(
        checked >= 30,
        "only {checked} hair parts seen — the sweep did not run"
    );
    assert_eq!(
        cross_race_authored, 0,
        "fig3 authors no cross-race hair mesh; {cross_race_authored} appeared, which means part \
         resolution is handing a body another race's mesh again (ledger E4)"
    );
    assert!(
        wrong.is_empty(),
        "{} avatar(s) had their authored hair mesh replaced with another race's:\n  {}",
        wrong.len(),
        wrong.join("\n  ")
    );
}

/// A character's own face and hair reach the assembled body.
///
/// This is the parity claim in one test: DOL stores `FaceType` / `HairStyle` / `HairColor` per
/// character and hands them back on the overview and on PlayerCreate, so two players of one race
/// are two different people. For as long as the assembler asked the look tables for index 1, every
/// Firbolg male in the world wore the same face, and no count anywhere read wrong — the parts
/// bound, the textures loaded, and the body was simply somebody else's.
///
/// Asserts on the BOUND TEXTURE NAMES rather than on a pixel count, because "a texture bound" and
/// "the right texture bound" score identically otherwise — the lesson this file's header records.
#[test]
fn a_character_wears_the_face_and_hair_it_was_given() {
    use caer_protocol::customization::Customization;

    let root = root();
    // SAFETY: single-threaded test entry; the asset loaders read the client root from env.
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 60_000.0)).expect("gpu init");
    let mut models = EntityModels::load().expect("entity models");

    // Firbolg male: a race whose face map spans several distinct sheets, so a difference is
    // visible in the names rather than resolving back to one file.
    const RACE: u8 = 10;
    const GENDER: u8 = caer_assets::figures::GENDER_MALE;

    let bind = |models: &mut EntityModels, gpu: &mut Gpu, c: Customization| {
        let id = models.ensure_avatar(gpu, RACE, GENDER, c, None);
        let info = models.last_avatar_stand();
        let head = info
            .bound_textures
            .iter()
            .find(|(part, _)| part.to_ascii_lowercase().contains("head"))
            .map(|(_, tex)| tex.clone());
        let hair = info
            .bound_textures
            .iter()
            .find(|(part, _)| part.to_ascii_lowercase().contains("hair"))
            .map(|(_, tex)| tex.clone());
        (id, head, hair)
    };

    let plain = bind(&mut models, &mut gpu, Customization::default());
    assert!(plain.0.is_some(), "Firbolg male must assemble at all");

    let other_face = bind(
        &mut models,
        &mut gpu,
        Customization {
            face_type: 3,
            ..Customization::default()
        },
    );
    assert_ne!(
        plain.1, other_face.1,
        "face type 3 must bind a different face sheet than the default"
    );
    assert_ne!(
        plain.0, other_face.0,
        "two looks are two meshes; sharing a model id means the cache cannot tell them apart"
    );

    let other_hair = bind(
        &mut models,
        &mut gpu,
        Customization {
            hair_color: 3,
            ..Customization::default()
        },
    );
    assert_ne!(
        plain.2, other_hair.2,
        "hair colour 3 must bind a different hair sheet than the default"
    );

    // The uncustomised character is the one DOL stores when nobody touched the form, and it has to
    // keep resolving exactly what shipped before any of this existed — index 1 in every table.
    assert_eq!(
        plain.1.as_deref().map(str::to_ascii_lowercase),
        Some("fir_m_head01.dds#opaque".to_string()),
        "an uncustomised Firbolg male still wears face 1"
    );
}

/// A dressed slot takes the GARMENT when the client ships one.
///
/// `mskins.csv` indexes two sheets for Briton legs: `cthLegs01_SS_Bri`, solid brown leggings, and
/// `cthLegs01_SS_bri_F`, a painted bare leg at alpha 62. Preferring the gendered spelling bound the
/// bare leg, and the report that came back was "Briton female has no pants" — which is exactly what
/// it looks like, because the sheet is a leg rather than a legging.
///
/// The Eden capture of Briton female shows brown leggings with a seam down the shin, so the opaque
/// sheet is the one being worn. Asserts on the bound NAME and on the source art's opacity, because
/// both candidates bind, decode and cover the mesh identically — only the art tells them apart.
#[test]
fn a_dressed_slot_takes_the_garment_the_client_ships() {
    let root = root();
    // SAFETY: single-threaded test entry; the asset loaders read the client root from env.
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 60_000.0)).expect("gpu init");
    let mut models = EntityModels::load().expect("entity models");

    const BRITON: u8 = 1;
    let female = caer_assets::figures::GENDER_FEMALE;
    models
        .ensure_avatar(
            &mut gpu,
            BRITON,
            female,
            caer_protocol::customization::Customization::default(),
            None,
        )
        .expect("Briton female must assemble");

    let legs = models
        .last_avatar_stand()
        .bound_textures
        .iter()
        .find(|(part, _)| part.to_ascii_lowercase().contains("legs"))
        .map(|(_, key)| key.clone())
        .expect("Briton female must have a legs slot bound");

    assert!(
        legs.to_ascii_lowercase()
            .starts_with("cthlegs01_ss_bri.dds"),
        "Briton female legs must wear the leggings, not the bare-leg sheet — bound {legs}"
    );

    let src = models
        .avatar_sheet(&legs)
        .expect("the bound sheet must decode through the binder's own index");
    let (_, _, px) = src.rgba8_mip0().expect("decodes to rgba8");
    let translucent = px.chunks_exact(4).filter(|p| p[3] != 255).count();
    assert_eq!(
        translucent, 0,
        "the sheet a leg slot wears is a garment and is opaque all the way through"
    );
}

/// **Skin tone consistency.** Every bare-limb part of a body must wear the same skin as its face.
///
/// Sampled over the GEOMETRY's own UVs, through `uv_coverage`, not over the page and not off the
/// screen. Both of the cheaper measurements are wrong here and both were tried: a page mean mixes
/// in atlas regions no triangle reads, and a screen mask built from `CAER_PART_TINT` puts 150k
/// pixels of Hibernian grass into the torso because green scenery matches a saturated green tint.
///
/// Which parts count as bare limb is not guessed either — it is `reads_as_overlay()`, this
/// codebase's own test for a sheet that never reaches opaque under its own triangles, which is
/// exactly what bare-limb art is.
///
/// Prints the whole table; asserts only that no body drifts further than the worst one measured,
/// so a regression shows up as a new name rather than as a number nobody recognises.
#[test]
fn a_body_wears_one_skin() {
    use caer_render::uv_coverage;

    let root = root();
    // SAFETY: single-threaded test entry; `EntityModels::load` reads the client root from env.
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 60_000.0)).expect("gpu init");
    let mut models = EntityModels::load().expect("entity models");

    println!(
        "\n{:<14} {:<7} {:<18} {:>18} {:>8}",
        "race", "gender", "bare-limb part", "sampled rgb", "d(face)"
    );
    let mut worst: Vec<(f32, String)> = Vec::new();

    for (race, label, wire) in legal_combinations() {
        let gender = caer_protocol::overview::fig3_gender_from_db(wire);
        let g = if wire == 0 { "male" } else { "female" };
        let cov = uv_coverage::avatar_coverage(&mut models, &mut gpu, race, gender);

        // The face is the reference: it is skin by definition and nothing else on the body is.
        let Some(face) = cov
            .iter()
            .find(|pc| {
                let n = pc.part.to_ascii_lowercase();
                n.contains("head") && !n.contains("headb") && pc.sampled.texels > 0
            })
            .map(|pc| pc.sampled.rgb_mean)
        else {
            continue;
        };

        for pc in &cov {
            let n = pc.part.to_ascii_lowercase();
            if pc.sampled.texels == 0 || n.contains("head") || n.contains("hair") {
                continue;
            }
            // **Known-imperfect discriminator, kept because it is stable and pinned.**
            // `reads_as_overlay()` is this codebase's test for a sheet that never reaches opaque
            // under its own triangles, which is what most bare-limb art is. It has one measured
            // FALSE NEGATIVE: Highlander male's `cthLegs01_SS_Hig_M` is bare skin *and* fully
            // opaque, so the part Matt reported is the one part this cannot see — see
            // `docs/evidence/race_parity_2026-08-24/`.
            //
            // Two replacements were tried and are worse. A hue-family test admits brown leather:
            // Briton's boots and body arrive at distance 190-215 because leather and skin share a
            // hue. A page mean admits atlas regions no triangle reads. Until something separates
            // smooth skin from structured leather, this rule plus a named exception is honest and
            // a cleverer-looking one would not be.
            let rgb = pc.sampled.rgb_mean;
            if !pc.sampled.reads_as_overlay() {
                continue;
            }
            let d = (0..3).map(|c| (rgb[c] - face[c]).abs()).sum::<f32>();
            println!(
                "{:<14} {:<7} {:<18} {:>5.0},{:>5.0},{:>5.0} {:>8.0}",
                label, g, pc.part, rgb[0], rgb[1], rgb[2], d
            );
            worst.push((d, format!("{label} {g} {}", pc.part)));
        }
    }

    worst.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    println!("\nworst drift:");
    for (d, who) in worst.iter().take(8) {
        println!("  {d:6.0}  {who}");
    }
    assert!(
        !worst.is_empty(),
        "no bare-limb parts found at all — the overlay test or the coverage walk has broken"
    );

    // **Pinned as a count, in both directions**, the way this file's other census does it. Four
    // bare-limb parts wear a skin more than 40 apart from their own face: Half Ogre male (69) and
    // female (58), Sylvan male (42) and female (43). Growing means a new body drifted; shrinking
    // means one was fixed and this number should be lowered deliberately, not discovered later.
    let drifting: Vec<&String> = worst
        .iter()
        .filter(|(d, _)| *d > 40.0)
        .map(|(_, who)| who)
        .collect();
    assert_eq!(
        drifting.len(),
        4,
        "bare-limb parts drifting >40 from their own face: {drifting:?}"
    );
    assert!(
        worst[0].0 < 80.0,
        "worst drift grew past the measured Half Ogre male: {} at {:.0}",
        worst[0].1,
        worst[0].0
    );
}

/// **Shoulder height, posed against bind.** Does the idle clip LIFT the shoulders, and by how much?
///
/// Needs no reference capture and no camera, which is what makes it worth having: comparing a
/// shoulder against a screenshot means comparing across two lenses and two customised characters,
/// and both of those confounds have already sent a measurement here the wrong way. Posed-minus-bind
/// on the same rig has neither.
///
/// Normalised by the bind head height so races of different stature are comparable — a Lurikeen and
/// a Troll cannot be compared in raw units.
#[test]
fn shoulder_lift_report() {
    let root = root();
    // SAFETY: single-threaded test entry.
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 60_000.0)).expect("gpu");
    let mut models = EntityModels::load().expect("entity models");

    println!(
        "\n{:<14} {:<7} {:>9} {:>9} {:>10} {:>10} {:>10}",
        "race", "gender", "bindZ", "posedZ", "lift", "clav/head", "posed/head"
    );
    let mut rows: Vec<(f32, String)> = Vec::new();
    for (race, label, wire) in legal_combinations() {
        let gender = caer_protocol::overview::fig3_gender_from_db(wire);
        let Some(model) = models.ensure_avatar(
            &mut gpu,
            race,
            gender,
            caer_protocol::customization::Customization::default(),
            None,
        ) else {
            continue;
        };
        let Some(rig) = models.skinned_rig(model) else {
            continue;
        };
        let Some(r) = rig.rigs.first() else { continue };
        let sk = &r.skeleton;
        let find = |needle: &str| {
            sk.bones
                .iter()
                .position(|b| b.name.to_ascii_lowercase().contains(needle))
        };
        let (Some(l), Some(rr), Some(head)) =
            (find(" l clavicle"), find(" r clavicle"), find("bip01 head"))
        else {
            continue;
        };
        let posed = sk.pose(&rig.clip, 0.0);
        let bind_z = (sk.bones[l].world_bind.2[2] + sk.bones[rr].world_bind.2[2]) / 2.0;
        let posed_z = (posed[l].2[2] + posed[rr].2[2]) / 2.0;
        let head_z = sk.bones[head].world_bind.2[2];
        let lift = posed_z - bind_z;
        let norm = if head_z.abs() > 1e-3 {
            lift / head_z
        } else {
            0.0
        };
        let g = if wire == 0 { "male" } else { "female" };
        // **The statistic that answers "the shoulders sit too high" is a RATIO, not a lift.**
        // How far up the body the shoulder sits, against this rig's own head — comparable across
        // a Lurikeen and a Troll, and unaffected by the lens, which is still unsolved.
        let ratio = if head_z.abs() > 1e-3 {
            bind_z / head_z
        } else {
            0.0
        };
        let posed_ratio = if head_z.abs() > 1e-3 {
            posed_z / head_z
        } else {
            0.0
        };
        println!(
            "{:<14} {:<7} {:>9.2} {:>9.2} {:>10.3} {:>10.4} {:>10.4}",
            label, g, bind_z, posed_z, lift, ratio, posed_ratio
        );
        let _ = norm;
        rows.push((ratio, format!("{label} {g}")));
    }
    rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    println!("\nhighest shoulders, as a fraction of the rig's own head height:");
    for (v, who) in rows.iter().take(6) {
        println!("  {v:+.4}  {who}");
    }
    assert!(!rows.is_empty(), "no rigged body produced a clavicle pair");
}

/// **Is the clavicle DRIVEN by the idle, or is it riding its parent?**
///
/// A shoulder that reads as a permanent shrug is a shoulder sitting at its bind rotation while the
/// spine beneath it poses. World-space height cannot see that — the clavicle moves either way,
/// because its parent moved — which is why the earlier shoulder report found nothing. This asks
/// the only question that separates the two: does the clip carry a track for that bone's id, and
/// how far does its own LOCAL rotation travel.
#[test]
fn clavicle_drive_report() {
    let root = root();
    // SAFETY: single-threaded test entry.
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 60_000.0)).expect("gpu");
    let mut models = EntityModels::load().expect("entity models");

    println!(
        "\n{:<14} {:<7} {:>7} {:>7} {:>10} {:>10}",
        "race", "gender", "L drv", "R drv", "L localdeg", "R localdeg"
    );
    let mut undriven: Vec<String> = Vec::new();
    for (race, label, wire) in legal_combinations() {
        let gender = caer_protocol::overview::fig3_gender_from_db(wire);
        let Some(model) = models.ensure_avatar(
            &mut gpu,
            race,
            gender,
            caer_protocol::customization::Customization::default(),
            None,
        ) else {
            continue;
        };
        let Some(rig) = models.skinned_rig(model) else {
            continue;
        };
        let Some(r) = rig.rigs.first() else { continue };
        let sk = &r.skeleton;
        let find = |needle: &str| {
            sk.bones
                .iter()
                .position(|b| b.name.to_ascii_lowercase().contains(needle))
        };
        let (Some(l), Some(rr)) = (find(" l clavicle"), find(" r clavicle")) else {
            continue;
        };
        // Driven = the clip carries a track keyed to this bone's own DAoC id.
        let driven = |i: usize| {
            sk.bones[i]
                .id
                .is_some_and(|id| rig.clip.tracks.contains_key(&id))
        };
        // How far the bone's own local frame turns away from bind, in degrees.
        let local_travel = |i: usize| {
            let posed = sk.pose(&rig.clip, 0.0);
            let pm = caer_assets::nif::xform_to_mat4(&posed[i]);
            let parent = sk.bones[i].parent;
            let pp = parent.map(|p| caer_assets::nif::xform_to_mat4(&posed[p]));
            let bm = caer_assets::nif::xform_to_mat4(&sk.bones[i].world_bind);
            let bp = parent.map(|p| caer_assets::nif::xform_to_mat4(&sk.bones[p].world_bind));
            // Compare the bone's X axis expressed in its PARENT's frame, posed vs bind: that
            // removes everything the parent chain contributed.
            let in_parent = |m: &[[f32; 4]; 4], par: &Option<[[f32; 4]; 4]>| {
                let ax = [m[0][0], m[0][1], m[0][2]];
                match par {
                    None => ax,
                    Some(p) => [
                        ax[0] * p[0][0] + ax[1] * p[0][1] + ax[2] * p[0][2],
                        ax[0] * p[1][0] + ax[1] * p[1][1] + ax[2] * p[1][2],
                        ax[0] * p[2][0] + ax[1] * p[2][1] + ax[2] * p[2][2],
                    ],
                }
            };
            let a = in_parent(&pm, &pp);
            let b = in_parent(&bm, &bp);
            let dot: f32 = (0..3).map(|c| a[c] * b[c]).sum();
            dot.clamp(-1.0, 1.0).acos().to_degrees()
        };
        let g = if wire == 0 { "male" } else { "female" };
        let (dl, dr) = (driven(l), driven(rr));
        println!(
            "{:<14} {:<7} {:>7} {:>7} {:>10.2} {:>10.2}",
            label,
            g,
            dl,
            dr,
            local_travel(l),
            local_travel(rr)
        );
        if !dl || !dr {
            undriven.push(format!("{label} {g}"));
        }
    }
    println!("\nbodies whose idle does NOT drive a clavicle: {undriven:?}");
    assert!(!undriven.is_empty() || undriven.is_empty(), "report only");
}

/// How each idle ENCODES the rotations we sample: quaternion keys, or per-axis Euler.
///
/// `i_vf` turns the Valkyn female's clavicles 22 degrees where every other body turns 11, and it
/// is the same clip whose neck angle came out the opposite sign to every other female. Two
/// anomalies on one clip is a decoding question before it is an art question.
#[test]
fn idle_rotation_channel_report() {
    use caer_assets::nif::RotChannel;

    let root = root();
    // SAFETY: single-threaded test entry.
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 60_000.0)).expect("gpu");
    let mut models = EntityModels::load().expect("entity models");

    println!(
        "\n{:<14} {:<7} {:>6} {:>6} {:>6} {:>14} {:>10}",
        "race", "gender", "quat", "euler", "none", "clavicle chan", "clav keys"
    );
    for (race, label, wire) in legal_combinations() {
        let gender = caer_protocol::overview::fig3_gender_from_db(wire);
        let Some(model) = models.ensure_avatar(
            &mut gpu,
            race,
            gender,
            caer_protocol::customization::Customization::default(),
            None,
        ) else {
            continue;
        };
        let Some(rig) = models.skinned_rig(model) else {
            continue;
        };
        let Some(r) = rig.rigs.first() else { continue };
        let sk = &r.skeleton;
        let (mut q, mut e, mut n) = (0usize, 0usize, 0usize);
        for t in rig.clip.tracks.values() {
            match &t.rotation {
                RotChannel::Quat(_) => q += 1,
                RotChannel::Euler { .. } => e += 1,
                RotChannel::None => n += 1,
            }
        }
        let clav = sk
            .bones
            .iter()
            .find(|b| b.name.to_ascii_lowercase().contains(" l clavicle"))
            .and_then(|b| b.id)
            .and_then(|id| rig.clip.tracks.get(&id));
        let (chan, keys) = match clav.map(|t| &t.rotation) {
            Some(RotChannel::Quat(k)) => ("quat", k.len()),
            Some(RotChannel::Euler { x, y, z }) => ("euler", x.len().max(y.len()).max(z.len())),
            Some(RotChannel::None) => ("none", 0),
            None => ("<no track>", 0),
        };
        let g = if wire == 0 { "male" } else { "female" };
        println!(
            "{:<14} {:<7} {:>6} {:>6} {:>6} {:>14} {:>10}",
            label, g, q, e, n, chan, keys
        );
    }
}

/// **Both wrists, or the arm tears on one side.**
///
/// `reparent_twist_bones` only moves a `ForeTwist` whose `Forearm` already precedes it in the
/// array, because the FK walk needs parents first. If a rig orders one side the other way round,
/// that side keeps the client's own UpperArm parent and the other does not — and an asymmetric
/// wrist is exactly what a body with one reparented arm looks like.
#[test]
fn every_forearm_twist_follows_its_forearm() {
    let root = root();
    // SAFETY: single-threaded test entry.
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 60_000.0)).expect("gpu");
    let mut models = EntityModels::load().expect("entity models");

    let mut bad: Vec<String> = Vec::new();
    println!(
        "\n{:<14} {:<7} {:<26} {:>6} {:>6}",
        "race", "gender", "twist bone", "idx", "parent"
    );
    for (race, label, wire) in legal_combinations() {
        let gender = caer_protocol::overview::fig3_gender_from_db(wire);
        let Some(model) = models.ensure_avatar(
            &mut gpu,
            race,
            gender,
            caer_protocol::customization::Customization::default(),
            None,
        ) else {
            continue;
        };
        let Some(rig) = models.skinned_rig(model) else {
            continue;
        };
        let Some(r) = rig.rigs.first() else { continue };
        let sk = &r.skeleton;
        for (i, b) in sk.bones.iter().enumerate() {
            if !b.name.ends_with("ForeTwist") {
                continue;
            }
            let want = b.name.trim_end_matches("ForeTwist").to_string() + "Forearm";
            let Some(&fa) = sk.name_to_index.get(&want) else {
                continue;
            };
            let ok = b.parent == Some(fa);
            let g = if wire == 0 { "male" } else { "female" };
            if !ok {
                let kids: Vec<&str> = sk
                    .bones
                    .iter()
                    .filter(|c| c.parent == Some(i))
                    .map(|c| c.name.as_str())
                    .collect();
                println!(
                    "{:<14} {:<7} {:<26} {:>6} {:>6}  forearm at {fa} — NOT reparented, children {kids:?}",
                    label,
                    g,
                    b.name,
                    i,
                    b.parent.map_or(-1i64, |p| p as i64)
                );
                bad.push(format!("{label} {g} {}", b.name));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} forearm twist bone(s) still hang off the wrong parent — that arm tears at the wrist:\n  {}",
        bad.len(),
        bad.join("\n  ")
    );
}

/// **Is the 22-degree Valkyn shoulder AUTHORED, or do we produce it?**
///
/// `sample_local` does not apply a clip rotation as a delta — it REPLACES the bone's bind local
/// rotation with the clip's. So the visible travel is the angle between the authored key and the
/// bind, and a clip authored against a different bind produces a different travel on this rig even
/// though the key is identical. This reports both halves so they can be told apart.
#[test]
fn clavicle_authored_key_versus_bind() {
    let root = root();
    // SAFETY: single-threaded test entry.
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 60_000.0)).expect("gpu");
    let mut models = EntityModels::load().expect("entity models");

    // Angle between two rotation matrices, in degrees.
    let angle = |a: &[f32; 9], b: &[f32; 9]| {
        // trace(aᵀb) = 1 + 2cos(theta)
        let mut t = 0.0f32;
        for r in 0..3 {
            for c in 0..3 {
                if r == c {
                    t += (0..3).map(|k| a[k * 3 + r] * b[k * 3 + c]).sum::<f32>();
                }
            }
        }
        (((t - 1.0) / 2.0).clamp(-1.0, 1.0)).acos().to_degrees()
    };

    println!(
        "\n{:<14} {:<7} {:>12} {:>12}",
        "race", "gender", "key-vs-bind", "bind-vs-Briton"
    );
    let mut briton_bind: Option<[f32; 9]> = None;
    for (race, label, wire) in legal_combinations() {
        let gender = caer_protocol::overview::fig3_gender_from_db(wire);
        let Some(model) = models.ensure_avatar(
            &mut gpu,
            race,
            gender,
            caer_protocol::customization::Customization::default(),
            None,
        ) else {
            continue;
        };
        let Some(rig) = models.skinned_rig(model) else {
            continue;
        };
        let Some(r) = rig.rigs.first() else { continue };
        let sk = &r.skeleton;
        let bone_of = |needle: &str| {
            sk.bones
                .iter()
                .position(|b| b.name.to_ascii_lowercase().contains(needle))
        };
        let g = if wire == 0 { "male" } else { "female" };
        // The whole shoulder chain, because a shrug is the upper arm as much as the clavicle.
        let mut chain = String::new();
        for needle in [" l clavicle", " l upperarm", " l forearm"] {
            let Some(bi) = bone_of(needle) else {
                chain.push_str("      -");
                continue;
            };
            let Some(id) = sk.bones[bi].id else {
                chain.push_str("      -");
                continue;
            };
            let Some(tr) = rig.clip.tracks.get(&id) else {
                chain.push_str("      -");
                continue;
            };
            let a = angle(
                &caer_assets::nif::sample_track_local(tr, 0.0, &sk.bones[bi].local).0,
                &sk.bones[bi].local.0,
            );
            chain.push_str(&format!("{a:7.2}"));
        }
        println!("{label:<14} {g:<7} chain(clav,upper,fore) {chain}");
        let Some(l) = bone_of(" l clavicle") else {
            continue;
        };
        let bind_rot = sk.bones[l].local.0;
        let Some(id) = sk.bones[l].id else { continue };
        let Some(track) = rig.clip.tracks.get(&id) else {
            continue;
        };
        // What the clip actually says at t=0, as a rotation.
        let posed_local = caer_assets::nif::sample_track_local(track, 0.0, &sk.bones[l].local);
        let key_vs_bind = angle(&posed_local.0, &bind_rot);
        if briton_bind.is_none() && label == "Briton" && wire == 0 {
            briton_bind = Some(bind_rot);
        }
        let bind_vs_briton = briton_bind.map_or(0.0, |b| angle(&bind_rot, &b));
        // Across the whole cycle, not just t=0: a shoulder that is shrugged at every phase is a
        // static pose, while one that only peaks is animation and a capture caught it mid-swing.
        let dur = if rig.clip.duration > 0.01 {
            rig.clip.duration
        } else {
            1.0
        };
        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        for k in 0..=20 {
            let t = dur * (k as f32) / 20.0;
            let a = angle(
                &caer_assets::nif::sample_track_local(track, t, &sk.bones[l].local).0,
                &bind_rot,
            );
            lo = lo.min(a);
            hi = hi.max(a);
        }
        println!(
            "   {label:<11} {g:<7} {key_vs_bind:>12.2} {bind_vs_briton:>12.2}   over cycle {lo:6.2}..{hi:6.2}  keys {}",
            match &track.rotation {
                caer_assets::nif::RotChannel::Quat(k) => k.len(),
                caer_assets::nif::RotChannel::Euler { x, .. } => x.len(),
                caer_assets::nif::RotChannel::None => 0,
            }
        );
    }
}

/// **What does the idle's TRANSLATION channel say, and what do we do with it?**
///
/// `sample_local` takes translation from the clip only for the root and its direct children
/// (`carries_root_motion`); every other bone holds its BIND translation, on the reasoning that a
/// limb's length is rigid and a clip's translation track for it is redundant. That reasoning is an
/// assumption about the art, not a reading of it, and "the shoulders do not drop at rest" is
/// exactly what discarding an authored shoulder translation would look like.
///
/// So: per body, run FK twice — once the way the renderer does it, once honouring every
/// translation track — and report where the two disagree.
#[test]
fn discarded_translation_report() {
    let root = root();
    // SAFETY: single-threaded test entry.
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 60_000.0)).expect("gpu");
    let mut models = EntityModels::load().expect("entity models");

    // The same composition `nif::xform_mul` performs, which is private to that crate.
    fn mat_mul(a: &[f32; 9], b: &[f32; 9]) -> [f32; 9] {
        let mut m = [0.0; 9];
        for r in 0..3 {
            for c in 0..3 {
                m[r * 3 + c] = (0..3).map(|k| a[r * 3 + k] * b[k * 3 + c]).sum();
            }
        }
        m
    }
    fn mat_apply(m: &[f32; 9], s: f32, v: [f32; 3]) -> [f32; 3] {
        [
            s * (m[0] * v[0] + m[1] * v[1] + m[2] * v[2]),
            s * (m[3] * v[0] + m[4] * v[1] + m[5] * v[2]),
            s * (m[6] * v[0] + m[7] * v[1] + m[8] * v[2]),
        ]
    }
    fn xmul(a: &caer_assets::nif::Xform, b: &caer_assets::nif::Xform) -> caer_assets::nif::Xform {
        let t = mat_apply(&a.0, a.1, b.2);
        (
            mat_mul(&a.0, &b.0),
            a.1 * b.1,
            [a.2[0] + t[0], a.2[1] + t[1], a.2[2] + t[2]],
        )
    }
    // Nearest-key lookup, deliberately not an interpolation: this asks what the channel HOLDS.
    fn trans_at(keys: &[caer_assets::nif::Key<3>], t: f32) -> Option<[f32; 3]> {
        if keys.is_empty() {
            return None;
        }
        let mut best = &keys[0];
        for k in keys {
            if (k.time - t).abs() < (best.time - t).abs() {
                best = k;
            }
        }
        Some(best.value)
    }

    println!(
        "\n{:<14} {:<7} {:>6} {:>8} {:>9} {:>9} {:>9}",
        "race", "gender", "tracks", "movedT", "clavdZ", "headdZ", "maxdZ"
    );
    let mut rows: Vec<(f32, String)> = Vec::new();
    for (race, label, wire) in legal_combinations() {
        let gender = caer_protocol::overview::fig3_gender_from_db(wire);
        let Some(model) = models.ensure_avatar(
            &mut gpu,
            race,
            gender,
            caer_protocol::customization::Customization::default(),
            None,
        ) else {
            continue;
        };
        let Some(rig) = models.skinned_rig(model) else {
            continue;
        };
        let Some(r) = rig.rigs.first() else { continue };
        let sk = &r.skeleton;

        // (a) the renderer's own pose, and (b) the same FK honouring every translation track.
        let posed = sk.pose(&rig.clip, 0.0);
        let mut with_t: Vec<caer_assets::nif::Xform> = Vec::with_capacity(sk.bones.len());
        let mut moved = 0usize;
        let mut tracks = 0usize;
        for (_i, b) in sk.bones.iter().enumerate() {
            let track = b.id.and_then(|id| rig.clip.tracks.get(&id));
            let mut local = match track {
                Some(tr) => caer_assets::nif::sample_track_local(tr, 0.0, &b.local),
                None => b.local,
            };
            if let Some(tr) = track {
                tracks += 1;
                if let Some(v) = trans_at(&tr.translation, 0.0) {
                    let d = ((v[0] - b.local.2[0]).powi(2)
                        + (v[1] - b.local.2[1]).powi(2)
                        + (v[2] - b.local.2[2]).powi(2))
                    .sqrt();
                    if d > 0.25 {
                        moved += 1;
                    }
                    local.2 = v;
                }
            }
            let w = match b.parent {
                Some(p) => xmul(&with_t[p], &local),
                None => local,
            };
            with_t.push(w);
        }

        let find = |needle: &str| {
            sk.bones
                .iter()
                .position(|b| b.name.to_ascii_lowercase().contains(needle))
        };
        let dz = |i: usize| with_t[i].2[2] - posed[i].2[2];
        let clav = find(" l clavicle").map_or(0.0, dz);
        let head = find("bip01 head").map_or(0.0, dz);
        let maxdz = (0..sk.bones.len())
            .map(|i| dz(i).abs())
            .fold(0.0f32, f32::max);
        let g = if wire == 0 { "male" } else { "female" };
        println!("{label:<14} {g:<7} {tracks:>6} {moved:>8} {clav:>9.2} {head:>9.2} {maxdz:>9.2}");
        // The two reported shoulder cases need a per-bone ledger before changing pose policy.
        // A whole-rig translation switch can make the shoulder look better by distorting fingers,
        // so identify the authored channels that actually move the shoulder chain.
        if (label == "Valkyn" && wire == 1) || (label == "Firbolg" && wire == 0) {
            println!("  {label} {g} non-bind translation keys at t=0:");
            for (i, bone) in sk.bones.iter().enumerate() {
                let Some(track) = bone.id.and_then(|id| rig.clip.tracks.get(&id)) else {
                    continue;
                };
                let Some(value) = trans_at(&track.translation, 0.0) else {
                    continue;
                };
                let delta = [
                    value[0] - bone.local.2[0],
                    value[1] - bone.local.2[1],
                    value[2] - bone.local.2[2],
                ];
                let length = (delta[0].powi(2) + delta[1].powi(2) + delta[2].powi(2)).sqrt();
                if length <= 0.25 {
                    continue;
                }
                let parent = bone
                    .parent
                    .and_then(|p| sk.bones.get(p))
                    .map_or("<root>", |b| b.name.as_str());
                println!(
                    "    {i:>3} {parent:<24} -> {:<24}  d=({:+.2},{:+.2},{:+.2})",
                    bone.name, delta[0], delta[1], delta[2]
                );
            }
        }
        rows.push((clav.abs(), format!("{label} {g}")));
    }
    rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    println!("\nlargest clavicle Z change if the discarded translation were honoured:");
    for (v, who) in rows.iter().take(8) {
        println!("  {v:8.2}  {who}");
    }
    assert!(!rows.is_empty(), "no rigged body produced a pose");
}

/// A/B contact sheet: the pose as the renderer builds it, beside the same pose with every
/// translation track honoured.
///
/// [`discarded_translation_report`] shows 22-34 of 35 driven bones carry a translation the
/// renderer throws away, and that honouring them moves Valkyn female's clavicle down 2.31 units
/// and Firbolg male's down 1.56 — the two bodies Matt reports as permanently shrugged. A number
/// that size is not a verdict, though: it moves Troll male's UP 3.59. The picture is the verdict.
///
/// Opt-in evidence generator for an explicitly chosen `CAER_TRANS_AB` output path.
///
/// Routine tests must not write screenshots into the source tree: Cargo runs this integration
/// test with the crate directory as its working directory, so a relative default used to create
/// an untracked duplicate under `crates/caer-render/docs/`.  Set `CAER_TRANS_AB` deliberately
/// when refreshing the A/B evidence.
#[test]
fn translation_ab_sheet() {
    let Ok(out) = std::env::var("CAER_TRANS_AB") else {
        return;
    };
    let root = root();
    // SAFETY: single-threaded test entry.
    std::env::set_var("CAER_CLIENT", &root);

    const W: u32 = 260;
    const H: u32 = 420;

    fn mat_mul(a: &[f32; 9], b: &[f32; 9]) -> [f32; 9] {
        let mut m = [0.0; 9];
        for r in 0..3 {
            for c in 0..3 {
                m[r * 3 + c] = (0..3).map(|k| a[r * 3 + k] * b[k * 3 + c]).sum();
            }
        }
        m
    }
    fn mat_apply(m: &[f32; 9], s: f32, v: [f32; 3]) -> [f32; 3] {
        [
            s * (m[0] * v[0] + m[1] * v[1] + m[2] * v[2]),
            s * (m[3] * v[0] + m[4] * v[1] + m[5] * v[2]),
            s * (m[6] * v[0] + m[7] * v[1] + m[8] * v[2]),
        ]
    }
    fn xmul(a: &caer_assets::nif::Xform, b: &caer_assets::nif::Xform) -> caer_assets::nif::Xform {
        let t = mat_apply(&a.0, a.1, b.2);
        (
            mat_mul(&a.0, &b.0),
            a.1 * b.1,
            [a.2[0] + t[0], a.2[1] + t[1], a.2[2] + t[2]],
        )
    }
    fn trans_at(keys: &[caer_assets::nif::Key<3>], t: f32) -> Option<[f32; 3]> {
        if keys.is_empty() {
            return None;
        }
        let mut best = &keys[0];
        for k in keys {
            if (k.time - t).abs() < (best.time - t).abs() {
                best = k;
            }
        }
        Some(best.value)
    }

    let mut gpu = pollster::block_on(Gpu::new_headless(W, H, 60_000.0)).expect("gpu init");
    let mut models = EntityModels::load().expect("entity models");
    caer_render::atmosphere::publish(caer_render::atmosphere::Atmosphere::load_for_region(
        &root,
        "sky_default",
    ));

    // The two reported offenders, then controls: a body the report says moves the other way, and
    // two that barely move at all.
    let picks: Vec<(&str, u8)> = vec![
        ("Valkyn", 1),
        ("Firbolg", 0),
        ("Troll", 0),
        ("Dwarf", 1),
        ("Briton", 0),
        ("Highlander", 0),
    ];
    let combos = legal_combinations();
    let cols = picks.len() as u32;
    let (sheet_w, sheet_h) = (cols * W, 2 * H);
    let mut sheet = vec![0u8; (sheet_w * sheet_h * 4) as usize];

    for (ci, (want, wire_want)) in picks.iter().enumerate() {
        let Some(&(race, label, wire)) =
            combos.iter().find(|(_, l, g)| l == want && g == wire_want)
        else {
            continue;
        };
        let gender = caer_protocol::overview::fig3_gender_from_db(wire);
        let Some(model) = models.ensure_avatar(
            &mut gpu,
            race,
            gender,
            caer_protocol::customization::Customization::default(),
            None,
        ) else {
            continue;
        };
        let scale = models.race_display_scale(race, gender);
        let height = models.model_height(model).unwrap_or(70.0);
        let Some(rig) = models.skinned_rig(model) else {
            continue;
        };

        // Row 1: exactly what the renderer builds. Row 2: same, translations honoured.
        let stock = caer_render::anim_skin::build_palettes(
            rig,
            &[caer_render::anim_skin::UniquePaletteJob {
                loco: caer_render::entities::Loco::Idle,
                t: 0.0,
                blend: None,
            }],
        );
        let mut honoured: Vec<[[f32; 4]; 4]> = Vec::with_capacity(stock.len());
        for r in &rig.rigs {
            let sk = &r.skeleton;
            let mut world: Vec<caer_assets::nif::Xform> = Vec::with_capacity(sk.bones.len());
            for b in &sk.bones {
                let track = b.id.and_then(|id| rig.clip.tracks.get(&id));
                let mut local = match track {
                    Some(tr) => caer_assets::nif::sample_track_local(tr, 0.0, &b.local),
                    None => b.local,
                };
                if let Some(v) = track.and_then(|tr| trans_at(&tr.translation, 0.0)) {
                    local.2 = v;
                }
                let w = match b.parent {
                    Some(p) => xmul(&world[p], &local),
                    None => local,
                };
                world.push(w);
            }
            honoured.extend(r.matrices_from_world(&world));
        }
        for m in &mut honoured {
            m[3][2] += rig.z_offset;
        }

        for (row, palettes) in [(0u32, &stock), (1u32, &honoured)] {
            gpu.clear_skinned_instances();
            gpu.clear_entity_instances();
            let back = height * 1.35 / (30.0_f32.to_radians()).tan();
            let mut cam = Camera::new(
                Vec3::new(0.0, -back, height * 0.55),
                Vec3::new(0.0, 0.0, height * 0.5),
                Vec3::ZERO,
                W as f32 / H as f32,
            );
            cam.ensure_far(60_000.0);
            gpu.set_view_proj(cam.view_proj().to_cols_array_2d());
            let inst = caer_render::gpu::SkinnedInstance {
                pos_yaw: [0.0, 0.0, 0.0, std::f32::consts::PI],
                scale,
                palette_base: 0.0,
            };
            gpu.update_skinned_instances(model, &[inst], palettes);
            let cell = gpu
                .render_to_rgba_pass(0, FramePass::PreWorldScene)
                .expect("cell render");
            let (cx, cy) = (ci as u32 * W, row * H);
            for y in 0..H {
                let src = (y * W * 4) as usize;
                let dst = (((cy + y) * sheet_w + cx) * 4) as usize;
                sheet[dst..dst + (W * 4) as usize]
                    .copy_from_slice(&cell[src..src + (W * 4) as usize]);
            }
        }
        println!(
            "{label} {} rendered both ways",
            if wire == 0 { "male" } else { "female" }
        );
    }

    common::write_png(&out, sheet_w, sheet_h, &sheet);
    println!("\ntop row = shipped pose, bottom row = translations honoured -> {out}");
}

#[test]
fn highlander_male_starter_uses_the_matching_unisex_legs() {
    let root = root();
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 60_000.0)).expect("gpu init");
    let mut models = EntityModels::load().expect("entity models");
    assert!(
        models
            .ensure_avatar(
                &mut gpu,
                3,
                caer_assets::figures::GENDER_MALE,
                caer_protocol::customization::Customization::default(),
                None,
            )
            .is_some(),
        "Highlander male must assemble"
    );
    let legs = models
        .last_avatar_stand()
        .bound_textures
        .iter()
        .find(|(part, _)| part.to_ascii_lowercase().contains("legs"))
        .map(|(_, texture)| texture.to_ascii_lowercase())
        .expect("Highlander male must bind a leg sheet");
    assert!(
        legs.contains("cthlegs01_ss_hig.dds#opaque"),
        "Highlander male starter must use the unisex leg tone, got {legs}"
    );
    assert!(
        !legs.contains("_hig_m.dds"),
        "the pale gendered Highlander leg duplicate must not win, got {legs}"
    );
}

#[test]
fn valkyn_character_screen_policy_relaxes_only_clavicles_and_keeps_the_authored_stoop() {
    let root = root();
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 60_000.0)).expect("gpu init");
    let mut models = EntityModels::load().expect("entity models");
    let model = models
        .ensure_avatar(
            &mut gpu,
            14,
            caer_assets::figures::GENDER_FEMALE,
            caer_protocol::customization::Customization::default(),
            None,
        )
        .expect("Valkyn female must assemble");
    let rig = models
        .skinned_rig(model)
        .expect("Valkyn female must be skinned");
    let skeleton = &rig.rigs[0].skeleton;
    let pose_policy = caer_render::entities::character_screen_pose_policy(
        14,
        caer_assets::figures::GENDER_FEMALE,
    )
    .expect("Valkyn female has a character-screen policy");
    let raw = skeleton.pose(&rig.clip, 0.0);
    let corrected = skeleton.pose_with_policy(&rig.clip, 0.0, pose_policy);
    let index = |name: &str| {
        skeleton
            .bones
            .iter()
            .position(|b| b.name.eq_ignore_ascii_case(name))
            .unwrap_or_else(|| panic!("missing {name}"))
    };
    let clavicle_angle = |bone_index: usize| {
        let bone = &skeleton.bones[bone_index];
        let track = rig
            .clip
            .tracks
            .get(&bone.id.expect("clavicle id"))
            .expect("clavicle track");
        let posed = caer_assets::nif::sample_track_local(track, 0.0, &bone.local);
        let mut trace = 0.0;
        for r in 0..3 {
            trace += (0..3)
                .map(|k| bone.local.0[k * 3 + r] * posed.0[k * 3 + r])
                .sum::<f32>();
        }
        (((trace - 1.0) / 2.0).clamp(-1.0, 1.0)).acos()
    };
    let left = index("Bip01 L Clavicle");
    let right = index("Bip01 R Clavicle");
    let spine = index("Bip01 Spine2");
    let head = index("Bip01 Head");
    let world_delta = |a: &caer_assets::nif::Xform, b: &caer_assets::nif::Xform| {
        let mut trace = 0.0;
        for r in 0..3 {
            trace += (0..3).map(|k| a.0[k * 3 + r] * b.0[k * 3 + r]).sum::<f32>();
        }
        (((trace - 1.0) / 2.0).clamp(-1.0, 1.0)).acos()
    };
    assert!(
        world_delta(&raw[left], &corrected[left]) > 0.05
            && world_delta(&raw[right], &corrected[right]) > 0.05,
        "the policy must actually alter both shoulder rotations"
    );
    let left_authored_degrees = clavicle_angle(left).to_degrees();
    let right_authored_degrees = clavicle_angle(right).to_degrees();
    assert!(
        left_authored_degrees > 20.0 && right_authored_degrees > 18.0,
        "this must exercise the authored Valkyn shrug rather than a generic clip \
         (left={left_authored_degrees:.2}°, right={right_authored_degrees:.2}°)"
    );
    assert_eq!(
        raw[spine], corrected[spine],
        "a clavicle-only correction must preserve the authored Valkyn spine/stoop"
    );
    assert_eq!(
        raw[head], corrected[head],
        "a clavicle-only correction must preserve the authored Valkyn head posture"
    );
    assert_eq!(
        rig.clip.duration, 4.0,
        "policy must not replace the Valkyn's authored idle with the generic 1-second clip"
    );
}

#[test]
fn firbolg_male_character_screen_policy_lowers_the_torso_without_stretching_hands() {
    let root = root();
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 60_000.0)).expect("gpu init");
    let mut models = EntityModels::load().expect("entity models");
    let model = models
        .ensure_avatar(
            &mut gpu,
            10,
            caer_assets::figures::GENDER_MALE,
            caer_protocol::customization::Customization::default(),
            None,
        )
        .expect("Firbolg male must assemble");
    let rig = models
        .skinned_rig(model)
        .expect("Firbolg male must be skinned");
    let skeleton = &rig.rigs[0].skeleton;
    let clavicles: Vec<usize> = skeleton
        .bones
        .iter()
        .enumerate()
        .filter(|(_, b)| b.name.to_ascii_lowercase().contains("clavicle"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        clavicles.len(),
        2,
        "Firbolg male must expose both clavicles"
    );
    let policy =
        caer_render::entities::character_screen_pose_policy(10, caer_assets::figures::GENDER_MALE)
            .expect("Firbolg male has a character-screen policy");
    let rigid = skeleton.pose(&rig.clip, 0.0);
    let authored = skeleton.pose_with_policy(&rig.clip, 0.0, policy);
    // Known-bad control: the former all-translation fix. It is retained only as a falsifier, not
    // as a production API, so this test goes red if a later cleanup widens the policy again.
    let every_track: Vec<i32> = rig.clip.tracks.keys().copied().collect();
    let all_translations = skeleton.pose_with_policy(
        &rig.clip,
        0.0,
        caer_assets::nif::PosePolicy {
            translation_ids: &every_track,
            rotation_weights: &[],
        },
    );
    let rigid_z: f32 = clavicles.iter().map(|&i| rigid[i].2[2]).sum::<f32>() / 2.0;
    let authored_z: f32 = clavicles.iter().map(|&i| authored[i].2[2]).sum::<f32>() / 2.0;
    assert!(
        authored_z < rigid_z - 0.20,
        "Firbolg upper-torso policy should lower the stand (rigid={rigid_z:.2}, selected={authored_z:.2})"
    );
    let index = |name: &str| {
        skeleton
            .bones
            .iter()
            .position(|b| b.name.eq_ignore_ascii_case(name))
            .unwrap_or_else(|| panic!("missing {name}"))
    };
    let hand = index("Bip01 L Hand");
    let finger = index("Bip01 L Finger1");
    let distance = |pose: &[caer_assets::nif::Xform]| {
        let a = pose[hand].2;
        let b = pose[finger].2;
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
    };
    let rigid_hand = distance(&rigid);
    let selected_hand = distance(&authored);
    let broad_hand = distance(&all_translations);
    assert!(
        (selected_hand - rigid_hand).abs() < 1e-3,
        "the selected torso chain must retain rigid hand/finger length: rigid={rigid_hand:.3}, selected={selected_hand:.3}"
    );
    assert!(
        (broad_hand - rigid_hand).abs() > 0.25,
        "known-bad all-translation control must expose the old hand deformation: rigid={rigid_hand:.3}, broad={broad_hand:.3}"
    );
}
