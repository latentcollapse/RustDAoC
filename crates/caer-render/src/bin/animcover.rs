//! `animcover` — measure animation-table coverage across every creature model (A.3.5 verification).
//!
//! Answers, headlessly and over the whole model table rather than the handful a screenshot shows:
//!
//!   * how many models resolve an anim set, an idle clip, and a clip file that actually ships;
//!   * how often the A.3.4 NIF-stem heuristic disagreed with the authoritative tables (the reason
//!     A.3.5 exists), and how many models it could never have covered at all;
//!   * whether each clip's parsed key timeline agrees with the table's authored `frames / fps`
//!     — an independent cross-check that the `.kfa` parser reads the same clip the client does.
//!
//!   animcover [--limit N]

use std::collections::{HashMap, HashSet};

use caer_assets::anims::{Action, AnimTables};
use caer_assets::monsters::MonsterModels;

fn main() {
    let limit: usize = std::env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "--limit")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(usize::MAX);

    let root = caer_render::terrain::client_root();
    let resolver = MonsterModels::load(root.join("gamedata.mpk")).expect("load monster tables");
    let tables = AnimTables::load(root.join("gamedata.mpk")).expect("load anim tables");

    // Index the shipped clips so we can tell "the table names a clip" from "the clip exists".
    let mut anims: HashMap<String, std::path::PathBuf> = HashMap::new();
    if let Ok(rd) = std::fs::read_dir(root.join("anims")) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("kfa")) {
                if let Some(s) = p.file_stem() {
                    anims.insert(s.to_string_lossy().to_ascii_lowercase(), p);
                }
            }
        }
    }

    let (mut with_set, mut with_idle, mut shipped, mut rigged) = (0usize, 0usize, 0usize, 0usize);
    // A.3.7: locomotion coverage — does a moving creature have a clip to play?
    let (mut has_walk, mut has_run, mut has_either, mut has_stride) =
        (0usize, 0usize, 0usize, 0usize);
    // A.3.10: directional locomotion — backing up and sidestepping have their own authored clips.
    let (mut has_back, mut has_slide_l, mut has_slide_r, mut has_dir_stride) =
        (0usize, 0usize, 0usize, 0usize);
    // Named-and-shipped is NOT the same as parses: a whole Gamebryo generation once failed to parse
    // while every coverage number looked healthy. So parse each distinct directional clip too.
    let (mut dir_parse_ok, mut dir_parse_fail) = (0usize, 0usize);
    let mut dir_checked: HashSet<String> = HashSet::new();
    let (mut heur_agree, mut heur_disagree, mut heur_absent) = (0usize, 0usize, 0usize);
    let (mut dur_checked, mut dur_agree) = (0usize, 0usize);
    let mut clip_use: HashMap<String, usize> = HashMap::new();
    let mut models: Vec<u16> = (0..u16::MAX)
        .filter(|m| resolver.nif_name(*m).is_some())
        .collect();
    models.truncate(limit);
    let total = models.len();

    // Parsing a clip is the expensive step; each distinct clip only needs checking once.
    let mut checked_clips: HashSet<String> = HashSet::new();

    for model in models {
        let nif = resolver.nif_name(model).unwrap().to_string();
        let Some(set) = resolver.anim_set(model) else {
            continue;
        };
        with_set += 1;
        let Some(cref) = tables
            .clip(set, Action::Idle)
            .or_else(|| tables.clip(set, Action::CombatIdle))
        else {
            continue;
        };
        with_idle += 1;
        *clip_use.entry(cref.stem.clone()).or_default() += 1;
        let Some(path) = anims.get(&cref.stem) else {
            continue;
        };
        shipped += 1;

        // Locomotion clips must both resolve from the table AND ship as a file.
        let ships = |a| {
            tables
                .clip(set, a)
                .map(|c| anims.contains_key(&c.stem))
                .unwrap_or(false)
        };
        let (w, r) = (ships(Action::Walk), ships(Action::Run));
        if w {
            has_walk += 1;
        }
        if r {
            has_run += 1;
        }
        if w || r {
            has_either += 1;
        }
        if resolver
            .strides(model)
            .is_some_and(|s| s.walk > 0.0 || s.run > 0.0)
        {
            has_stride += 1;
        }

        // Directional clips: resolve + ship, then actually PARSE each distinct one.
        if ships(Action::Back) {
            has_back += 1;
        }
        if ships(Action::SlideLeft) {
            has_slide_l += 1;
        }
        if ships(Action::SlideRight) {
            has_slide_r += 1;
        }
        if resolver
            .strides(model)
            .is_some_and(|s| s.back > 0.0 || s.strafe > 0.0)
        {
            has_dir_stride += 1;
        }
        for a in [Action::Back, Action::SlideLeft, Action::SlideRight] {
            let Some(c) = tables.clip(set, a) else {
                continue;
            };
            let Some(p) = anims.get(&c.stem) else {
                continue;
            };
            if !dir_checked.insert(c.stem.clone()) {
                continue;
            }
            match std::fs::read(p)
                .ok()
                .map(|b| caer_assets::nif::read_clip(&b))
            {
                Some(Ok(_)) => dir_parse_ok += 1,
                _ => {
                    dir_parse_fail += 1;
                    println!("  directional clip FAILED to parse: {} ({a:?})", c.stem);
                }
            }
        }

        // What would the old A.3.4 stem heuristic have picked for this model?
        let base = nif.trim_end_matches(|c: char| c.is_ascii_digit());
        let base = if base.is_empty() { nif.as_str() } else { base };
        let guess = ["_cidle", "_idle"]
            .iter()
            .map(|s| format!("{base}{s}"))
            .find(|k| anims.contains_key(k));
        match guess {
            None => heur_absent += 1,
            Some(g) if g == cref.stem => heur_agree += 1,
            Some(_) => heur_disagree += 1,
        }

        // Does the mesh actually carry a skin (so posing applies at all)?
        if let Some(p) = client_nif(&root, &nif) {
            if let Ok(bytes) = std::fs::read(&p) {
                if matches!(caer_assets::nif::read_rigged(&bytes), Ok(Some(_))) {
                    rigged += 1;
                }
            }
        }

        // Cross-check the parsed timeline against the table's authored frames/fps, once per clip.
        if checked_clips.insert(cref.stem.clone()) {
            if let Ok(bytes) = std::fs::read(path) {
                if let Ok(clip) = caer_assets::nif::read_clip(&bytes) {
                    dur_checked += 1;
                    // Compare against the AUTHORED duration (frames / base_fps) — the key timeline's
                    // own rate. Comparing against playback fps is what made this check misfire.
                    let authored = cref.authored_duration();
                    // One frame of slack: the table counts frames, the keys span frame centres.
                    let slack = if cref.base_fps > 0.0 {
                        1.0 / cref.base_fps
                    } else {
                        0.05
                    };
                    if (clip.duration - authored).abs() <= slack + 1e-3 {
                        dur_agree += 1;
                    } else {
                        println!("  duration mismatch: {:<20} parsed {:.2}s vs authored {:.2}s ({} frames @ base {} fps, played {} fps)",
                            cref.stem, clip.duration, authored, cref.frames, cref.base_fps, cref.fps);
                    }
                }
            }
        }
    }

    let pct = |n: usize| {
        if total > 0 {
            n as f64 * 100.0 / total as f64
        } else {
            0.0
        }
    };
    println!("\n=== A.3.5 animation coverage over {total} resolvable creature models ===");
    println!(
        "  anim set resolved     : {with_set:5} ({:.1}%)",
        pct(with_set)
    );
    println!(
        "  idle clip resolved    : {with_idle:5} ({:.1}%)",
        pct(with_idle)
    );
    println!(
        "  clip file ships       : {shipped:5} ({:.1}%)",
        pct(shipped)
    );
    println!(
        "  ...and mesh is rigged : {rigged:5} ({:.1}%)  <- these actually pose",
        pct(rigged)
    );
    println!("\n  vs the old A.3.4 NIF-stem heuristic (of {shipped} table-resolved models):");
    println!("    heuristic agreed    : {heur_agree:5}");
    println!("    heuristic WRONG clip: {heur_disagree:5}");
    println!("    heuristic found none: {heur_absent:5}  <- would have stayed T-posed");
    println!("\n  clip timeline vs authored frames/fps: {dur_agree}/{dur_checked} agree");
    println!("\n=== A.3.7 locomotion coverage (of {shipped} animated models) ===");
    println!(
        "  walk clip ships       : {has_walk:5} ({:.1}%)",
        pct(has_walk)
    );
    println!(
        "  run clip ships        : {has_run:5} ({:.1}%)",
        pct(has_run)
    );
    println!(
        "  at least one          : {has_either:5} ({:.1}%)  <- these animate when moving",
        pct(has_either)
    );
    println!(
        "  stride data present   : {has_stride:5} ({:.1}%)  <- these keep feet planted",
        pct(has_stride)
    );

    println!("\n=== A.3.10 directional locomotion (of {shipped} animated models) ===");
    println!(
        "  back clip ships       : {has_back:5} ({:.1}%)",
        pct(has_back)
    );
    println!(
        "  slide-left ships      : {has_slide_l:5} ({:.1}%)",
        pct(has_slide_l)
    );
    println!(
        "  slide-right ships     : {has_slide_r:5} ({:.1}%)",
        pct(has_slide_r)
    );
    println!(
        "  back/strafe stride    : {has_dir_stride:5} ({:.1}%)",
        pct(has_dir_stride)
    );
    println!("  distinct clips PARSED : {dir_parse_ok:5} ok, {dir_parse_fail} failed  <- named+shipped is not parsed");

    let mut top: Vec<_> = clip_use.into_iter().collect();
    top.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    println!("\n  most-shared idle clips (proof clips are retargeted, not per-creature):");
    for (stem, n) in top.iter().take(8) {
        println!("    {n:5} models  {stem}");
    }
}

/// Locate a figures NIF by base name (the directory listing is case-preserving on disk).
fn client_nif(root: &std::path::Path, stem: &str) -> Option<std::path::PathBuf> {
    let dir = root.join("figures");
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_stem()
                .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(stem))
                && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("nif"))
        })
}
