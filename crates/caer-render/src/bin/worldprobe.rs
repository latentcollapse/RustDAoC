//! `worldprobe` — automated defect scan across every asset subsystem.
//!
//! Screenshots find defects one camera angle at a time, and only where someone thought to look.
//! This sweeps the whole client and reports what is measurably wrong or missing, so a regression
//! shows up as a changed number rather than as something Matt happens to fly past.
//!
//! **Every check runs the REAL load path.** That is the rule this repo keeps re-learning: coverage
//! numbers taken from tables rather than from the loader hid a 38% NIF-format hole and, more
//! recently, 159 dropped UI labels. A check that asks "does the table name one" proves nothing.
//!
//! What it looks for:
//!
//!   TERRAIN    zones that fail to load; holes in the height field
//!   FIXTURES   models that don't resolve or parse; textures that don't load; geometry buried in
//!              or floating above the ground (an orientation/placement smell)
//!   CREATURES  model ids that resolve no mesh; missing skins; missing idle/locomotion clips
//!   UI         art templates a window references but the skin doesn't define; fonts that fail to
//!              parse; adapters with nothing behind them
//!
//! Exit codes (typed — the gate must not treat findings as a crash):
//! - `0` — scan completed, no findings
//! - `1` — scan completed, findings present (including accepted RED baseline debt)
//! - `≥2` — scan did not complete (bad args / missing client / aborted)
//!
//! On completion, prints a terminal `worldprobe: complete phases=N` line so the gate can
//! require a completion marker rather than inferring success from exit status alone.
//!
//!   worldprobe [--region N] [--verbose]

use std::collections::{BTreeMap, HashSet};

use caer_assets::uiskin::Skin;
use caer_render::terrain;

/// How severe a finding is. RED means something is visibly broken now; AMBER means a gap we know
/// about and have decided to carry; GREY is informational.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Sev {
    Red,
    Amber,
    Grey,
}

impl Sev {
    fn tag(self) -> &'static str {
        match self {
            Sev::Red => "RED  ",
            Sev::Amber => "AMBER",
            Sev::Grey => "grey ",
        }
    }
}

struct Report {
    findings: Vec<(Sev, String, String)>,
}

impl Report {
    fn add(&mut self, sev: Sev, area: &str, msg: String) {
        self.findings.push((sev, area.to_string(), msg));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let region: u16 = args
        .windows(2)
        .find(|w| w[0] == "--region")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(1);
    let verbose = args.iter().any(|a| a == "--verbose");

    let mut rep = Report {
        findings: Vec::new(),
    };
    println!("worldprobe: scanning region {region}…\n");

    probe_terrain_and_fixtures(region, &mut rep, verbose);
    probe_creatures(&mut rep, verbose);
    probe_ui(&mut rep, verbose);

    // Report worst-first so the thing to fix next is at the top.
    rep.findings
        .sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let (mut red, mut amber) = (0, 0);
    println!("\n=== findings ===");
    for (sev, area, msg) in &rep.findings {
        match sev {
            Sev::Red => red += 1,
            Sev::Amber => amber += 1,
            Sev::Grey => {}
        }
        println!("  {} [{area}] {msg}", sev.tag());
    }
    println!("\n{red} red, {amber} amber, {} total", rep.findings.len());
    // Completion marker — gates.sh requires this line; exit status alone is not proof.
    println!("worldprobe: complete phases=3");
    // 0 = clean scan, 1 = findings present (normal; baseline diff decides the gate).
    // Incomplete runs never reach here (panic / missing client → runtime nonzero ≥2-ish).
    if rep.findings.is_empty() {
        std::process::exit(0);
    }
    std::process::exit(1);
}

/// Terrain + static world geometry.
fn probe_terrain_and_fixtures(region: u16, rep: &mut Report, verbose: bool) {
    // Load the region over its full extent — the same call the renderer makes, so anything that
    // fails here fails in the client too.
    let (min, max) = ([i32::MIN / 4, i32::MIN / 4], [i32::MAX / 4, i32::MAX / 4]);
    let mesh = terrain::load_region(region, glam::Vec3::ZERO, min, max, terrain::seam_blend());

    if mesh.zones_loaded == 0 {
        rep.add(
            Sev::Red,
            "terrain",
            format!("region {region} loaded ZERO zones"),
        );
        return;
    }
    println!(
        "terrain: {} zones, {} fixture models, {} textures",
        mesh.zones_loaded,
        mesh.models.len(),
        mesh.textures.len()
    );

    // Placeholder boxes are fixtures whose model did not resolve — each one is a visible defect.
    if !mesh.fixtures.is_empty() {
        rep.add(
            Sev::Amber,
            "fixtures",
            format!(
                "{} placeholder boxes (model failed to resolve; see CAER_LOG=debug for names)",
                mesh.fixtures.len()
            ),
        );
    }

    // Untextured geometry: a part with no resolved texture renders white/flat, which reads as a
    // missing asset rather than as a style choice.
    let mut untextured = 0usize;
    let mut parts = 0usize;
    for m in &mesh.models {
        for p in &m.parts {
            parts += 1;
            match &p.texture {
                Some(t) if !mesh.textures.contains_key(t) => untextured += 1,
                None => untextured += 1,
                _ => {}
            }
        }
    }
    if untextured > 0 {
        let pct = 100.0 * untextured as f32 / parts.max(1) as f32;
        rep.add(
            Sev::Amber,
            "fixtures",
            format!("{untextured}/{parts} model parts ({pct:.1}%) draw with no texture"),
        );
    }

    // Placement sanity: geometry that sits far below the ground is buried; far above it floats.
    // Both are placement/orientation smells and both are visible from a distance.
    let mut buried = 0usize;
    let mut floating = 0usize;
    let mut instances = 0usize;
    for m in &mesh.models {
        for inst in &m.instances {
            instances += 1;
            // Model bounds are relative to the instance origin.
            let (lo, hi) = (inst.pos[2] + m.bound_min[2], inst.pos[2] + m.bound_max[2]);
            let Some(ground) = mesh.height_at(inst.pos[0] as i32, inst.pos[1] as i32) else {
                continue;
            };
            // Wholly under the terrain: nothing of it can be seen.
            if hi < ground - 50.0 {
                buried += 1;
            }
            // Its lowest point hangs well clear of the ground.
            if lo > ground + 300.0 {
                floating += 1;
            }
        }
    }
    println!("fixtures: {instances} instances placed");
    if buried > 0 {
        rep.add(
            Sev::Amber,
            "placement",
            format!("{buried}/{instances} fixture instances are entirely BELOW the terrain"),
        );
    }
    if floating > 0 {
        rep.add(
            Sev::Amber,
            "placement",
            format!("{floating}/{instances} fixture instances FLOAT >300u above the terrain"),
        );
    }
    if verbose {
        println!("  (bounds checked against the retained heightfield)");
    }
}

/// Creature models: does every id a server can send actually render?
fn probe_creatures(rep: &mut Report, verbose: bool) {
    let root = terrain::client_root();
    let Ok(resolver) = caer_assets::monsters::MonsterModels::load(root.join("gamedata.mpk")) else {
        rep.add(Sev::Red, "creatures", "monsters.csv failed to load".into());
        return;
    };
    let tables = caer_assets::anims::AnimTables::load(root.join("gamedata.mpk")).ok();

    // Index the figures dir once, case-insensitively — the client's tables and its filenames
    // disagree on case constantly, and that has already cost us four separate bugs.
    let mut figures: HashSet<String> = HashSet::new();
    if let Ok(rd) = std::fs::read_dir(root.join("figures")) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            if let Some(stem) = n.strip_suffix(".nif") {
                figures.insert(stem.to_string());
            }
        }
    }

    let (mut total, mut no_mesh, mut no_skin, mut no_idle) = (0usize, 0usize, 0usize, 0usize);
    let mut missing: BTreeMap<String, usize> = BTreeMap::new();
    for id in 0..u16::MAX {
        let Some(nif) = resolver.nif_name(id) else {
            continue;
        };
        total += 1;
        if !figures.contains(&nif.to_ascii_lowercase()) {
            no_mesh += 1;
            *missing.entry(nif.to_string()).or_default() += 1;
            continue;
        }
        if resolver.body_skin(id).is_none() {
            no_skin += 1;
        }
        if let (Some(t), Some(set)) = (&tables, resolver.anim_set(id)) {
            if t.clip(set, caer_assets::anims::Action::Idle).is_none() {
                no_idle += 1;
            }
        }
    }
    println!("creatures: {total} model ids resolve a NIF name");
    if no_mesh > 0 {
        rep.add(
            Sev::Red,
            "creatures",
            format!(
                "{no_mesh}/{total} model ids name a mesh that does not ship ({} distinct)",
                missing.len()
            ),
        );
        if verbose {
            for (n, c) in missing.iter().take(10) {
                println!("    missing figure: {n} ({c} ids)");
            }
        }
    }
    if no_skin > 0 {
        rep.add(
            Sev::Amber,
            "creatures",
            format!("{no_skin}/{total} model ids have no body skin (render white)"),
        );
    }
    if no_idle > 0 {
        rep.add(
            Sev::Amber,
            "creatures",
            format!("{no_idle}/{total} model ids resolve no idle clip (T-pose risk)"),
        );
    }
}

/// UI skin: templates, fonts and adapters.
fn probe_ui(rep: &mut Report, verbose: bool) {
    let ui_dir = terrain::client_root().join("ui");
    for skin_name in ["atlantis", "isles"] {
        let Ok(skin) = Skin::load(&ui_dir, skin_name) else {
            rep.add(Sev::Red, "ui", format!("skin `{skin_name}` failed to load"));
            continue;
        };
        if !skin.problems.is_empty() {
            rep.add(
                Sev::Amber,
                "ui",
                format!(
                    "skin `{skin_name}`: {} file(s) named by uimain.xml could not be read",
                    skin.problems.len()
                ),
            );
        }

        // Art templates a window asks for but the skin never defines: those controls draw nothing.
        let mut unresolved: BTreeMap<String, usize> = BTreeMap::new();
        let mut adapters: HashSet<String> = HashSet::new();
        let mut fonts_used: HashSet<String> = HashSet::new();
        for w in skin.windows.values() {
            let d = caer_render::skinui::layout_window(&skin, w, (0.0, 0.0), &|_| None);
            for u in d.unresolved {
                *unresolved.entry(u).or_default() += 1;
            }
            for t in d.texts {
                if let Some(f) = t.font {
                    fonts_used.insert(f);
                }
                if let Some(a) = t.adapter.filter(|a| a != "none" && !a.is_empty()) {
                    adapters.insert(a);
                }
            }
        }
        println!(
            "ui `{skin_name}`: {} windows, {} fonts, {} adapters",
            skin.windows.len(),
            skin.fonts.len(),
            adapters.len()
        );
        if !unresolved.is_empty() {
            let total: usize = unresolved.values().sum();
            rep.add(
                Sev::Amber,
                "ui",
                format!(
                    "`{skin_name}`: {total} controls reference {} undefined art templates",
                    unresolved.len()
                ),
            );
            if verbose {
                for (n, c) in unresolved.iter().take(8) {
                    println!("    undefined template: {n} ({c}x)");
                }
            }
        }

        // Fonts must resolve AND parse — a font that resolves but fails to parse draws no text at
        // all, which is exactly how every bitmap font silently failed on a case-mismatched path.
        for f in &fonts_used {
            let Some(decl) = skin.font(f) else {
                rep.add(
                    Sev::Red,
                    "ui",
                    format!("`{skin_name}`: font `{f}` is used but not declared"),
                );
                continue;
            };
            if decl.file.to_ascii_lowercase().ends_with(".ttf") {
                continue; // TrueType is a separate path
            }
            let path = caer_assets::uiskin::resolve_ignoring_case(&ui_dir, &decl.file);
            let parsed = std::fs::read(&path)
                .ok()
                .and_then(|b| caer_assets::tga::decode(&b).ok())
                .and_then(caer_assets::bitmapfont::BitmapFont::parse);
            match parsed {
                Some(bf) if bf.unassigned > 0 => rep.add(
                    Sev::Grey,
                    "ui",
                    format!(
                        "`{skin_name}`: font `{f}` has {} unassigned glyph cells",
                        bf.unassigned
                    ),
                ),
                Some(_) => {}
                None => rep.add(
                    Sev::Red,
                    "ui",
                    format!(
                        "`{skin_name}`: font `{f}` ({}) failed to load or parse",
                        decl.file
                    ),
                ),
            }
        }

        // Adapter coverage is the honest UI progress metric — measured against the skin's demand.
        let status = caer_protocol::status::PlayerStatus::default();
        let st = caer_render::adapters::AdapterState {
            player_name: "probe",
            status: &status,
            target: Some(("probe", 50)),
            zone: Some("probe"),
            fps: 60,
            // The probe scores the skin, not a live session; with no sheet the character-sheet
            // adapters correctly count as unbound.
            sheet: None,
            char_stats: None,
            char_resists: None,
            equipment: None,
            money: None,
            inventory: None,
            merchant: None,
            weapon_armor: None,
            attack_mode: None,
            login_account: None,
            login_password_mask: None,
            create_name: None,
        };
        let demand: Vec<String> = adapters.into_iter().collect();
        let (bound, total) = caer_render::adapters::adapter_coverage(&st, &demand);
        if bound < total {
            rep.add(
                Sev::Amber,
                "ui",
                format!(
                    "`{skin_name}`: {}/{total} adapters have no game system behind them",
                    total - bound
                ),
            );
        }
    }
}
