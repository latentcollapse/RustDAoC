//! `uicover` — load a real UI skin and report what parsed.
//!
//! The point is the same as `animcover`: a parser that passes hand-written unit tests has proved
//! nothing about the 98 files the client actually ships. This runs the real load path over a real
//! skin and reports the windows, controls and fonts it got, plus every file it could not read.
//!
//!   uicover [skin]        (default: atlantis)

use std::collections::BTreeMap;

use caer_assets::uiskin::{Control, Skin};

fn main() {
    let skin_name = std::env::args().nth(1).unwrap_or_else(|| "atlantis".into());
    // Same convention as the renderer's `client_root`: CAER_CLIENT overrides, else the Wine prefix.
    let root = std::env::var("CAER_CLIENT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".wine/drive_c/Program Files (x86)/RustDAoC")
        });
    let ui_dir = root.join("ui");

    let skin = match Skin::load(&ui_dir, &skin_name) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "uicover: could not load {}/{skin_name}: {e}",
                ui_dir.display()
            );
            std::process::exit(1);
        }
    };

    let mut labels = 0usize;
    let mut images = 0usize;
    let mut other: BTreeMap<String, usize> = BTreeMap::new();
    let mut adapters: BTreeMap<String, usize> = BTreeMap::new();
    let mut fonts_used: BTreeMap<String, usize> = BTreeMap::new();
    let mut art: BTreeMap<String, usize> = BTreeMap::new();
    let mut buttons = 0usize;
    let mut edit_boxes = 0usize;

    for w in skin.windows.values() {
        for c in &w.controls {
            match c {
                Control::Label(l) => {
                    labels += 1;
                    if let Some(a) = &l.adapter {
                        *adapters.entry(a.clone()).or_default() += 1;
                    }
                    if let Some(f) = &l.font {
                        *fonts_used.entry(f.clone()).or_default() += 1;
                    }
                }
                Control::Image(i) => {
                    images += 1;
                    if let Some(t) = &i.template_name {
                        *art.entry(t.clone()).or_default() += 1;
                    }
                }
                Control::Button(b) => {
                    buttons += 1;
                    if let Some(t) = &b.template_name {
                        *art.entry(t.clone()).or_default() += 1;
                    }
                }
                Control::EditBox(e) => {
                    edit_boxes += 1;
                    if let Some(a) = &e.adapter {
                        *adapters.entry(a.clone()).or_default() += 1;
                    }
                }
                Control::Other { tag, .. } => *other.entry(tag.clone()).or_default() += 1,
            }
        }
    }

    println!("=== skin `{skin_name}` ===");
    println!("  windows parsed : {}", skin.windows.len());
    println!("  fonts declared : {}", skin.fonts.len());
    println!(
        "  controls       : {labels} labels, {images} images, {buttons} buttons, {edit_boxes} fields, {} other",
        other.values().sum::<usize>()
    );
    println!("  distinct art templates referenced : {}", art.len());
    println!("  texture pages  : {}", skin.textures.len());
    println!(
        "  image areas    : {}  (flat atlas rects)",
        skin.image_areas.len()
    );
    println!(
        "  nine-slices    : {}  (resizable window frames)",
        skin.nine_slices.len()
    );
    // The check that matters: can every referenced art template actually be resolved?
    let unresolved: Vec<&String> = art
        .keys()
        .filter(|t| skin.image_area(t).is_none() && skin.nine_slice(t).is_none())
        .collect();
    println!(
        "  art templates referenced but NOT defined : {}",
        unresolved.len()
    );
    for t in unresolved.iter().take(8) {
        println!("      {t}");
    }
    // And do the defined ones point at a real texture page?
    let bad_tex: Vec<String> = skin
        .image_areas
        .values()
        .map(|a| (&a.name, &a.texture))
        .chain(skin.nine_slices.values().map(|n| (&n.name, &n.texture)))
        .filter(|(_, t)| skin.texture(t).is_none())
        .map(|(n, t)| format!("{n} -> {t}"))
        .collect();
    println!(
        "  art templates naming a MISSING texture page : {}",
        bad_tex.len()
    );
    for b in bad_tex.iter().take(5) {
        println!("      {b}");
    }
    println!("\n  unmodelled template kinds (counted, not dropped):");
    let mut ut: Vec<_> = skin.unmodelled_templates.iter().collect();
    ut.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (tag, n) in ut.iter().take(8) {
        println!("    {n:5}  {tag}");
    }
    println!(
        "  distinct adapters (skin -> game state bindings) : {}",
        adapters.len()
    );

    // A font a control asks for but assets.xml never declared would render as nothing.
    // Case-insensitively, the way a control's reference actually resolves.
    let missing: Vec<&String> = fonts_used
        .keys()
        .filter(|f| skin.font(f).is_none())
        .collect();
    println!("  fonts referenced but NOT declared : {}", missing.len());
    for f in missing.iter().take(8) {
        println!("      {f}");
    }

    println!("\n  unmodelled control types (kept, not dropped):");
    for (tag, n) in other.iter().take(12) {
        println!("    {n:5}  {tag}");
    }

    println!("\n  most-used adapters — these are the game-state hooks the HUD must supply:");
    let mut top: Vec<_> = adapters.into_iter().collect();
    top.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (a, n) in top.iter().take(12) {
        println!("    {n:5}  {a}");
    }

    if skin.problems.is_empty() {
        println!("\n  no unreadable files");
    } else {
        println!("\n  PROBLEMS ({}):", skin.problems.len());
        for p in skin.problems.iter().take(20) {
            println!("    {p}");
        }
    }
}
