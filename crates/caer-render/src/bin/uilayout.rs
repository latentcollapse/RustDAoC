//! `uilayout` — lay out real skin windows, and software-composite one to an image.
//!
//! Two jobs, both about proving the layout against the client's own data rather than my fixtures:
//!
//!  * sweep every window in a skin, reporting quads/texts produced and any art template that fails
//!    to resolve — the coverage number for the layout stage;
//!  * `--render <window> <out.rgba>` composites one window on the CPU, so the nine-slice geometry
//!    and the TGA decode can be LOOKED AT before any GPU pipeline exists.
//!
//! That second mode is the de-risk pattern the GPU skinning work used: prove the maths on the CPU
//! first, and a later visual difference on the GPU is provably a binding bug rather than geometry.
//!
//!   uilayout [skin] [--render <window> <out.rgba>]

use std::collections::HashMap;

use caer_assets::tga::TgaImage;
use caer_assets::uiskin::Skin;
use caer_render::skinui::{layout_window, UiQuad};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let skin_name = args
        .get(1)
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| "atlantis".into());
    let root = caer_render::terrain::client_root();
    let ui_dir = root.join("ui");

    let skin = match Skin::load(&ui_dir, &skin_name) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("uilayout: {}: {e}", ui_dir.display());
            std::process::exit(1);
        }
    };

    if let Some(i) = args.iter().position(|a| a == "--render") {
        let (Some(win), Some(out)) = (args.get(i + 1), args.get(i + 2)) else {
            eprintln!("usage: uilayout [skin] --render <window> <out.rgba>");
            std::process::exit(2);
        };
        // Texture/font `File` values are relative to `ui/`, NOT to the skin directory — they read
        // `atlantis/atlantis_03.tga` and `fonts/arial11.tga`. Joining against the skin dir yields
        // `ui/atlantis/atlantis/...` and every page silently fails to load.
        render_one(&skin, &ui_dir, win, out);
        return;
    }

    // Coverage sweep: lay every window out and see what fails to resolve.
    let (mut quads, mut texts, mut adapter_labels) = (0usize, 0usize, 0usize);
    let mut unresolved: HashMap<String, usize> = HashMap::new();
    let mut names: Vec<&String> = skin.windows.keys().collect();
    names.sort();
    for n in &names {
        let w = &skin.windows[*n];
        let d = layout_window(&skin, w, (0.0, 0.0), &|_| None);
        quads += d.quads.len();
        texts += d.texts.len();
        adapter_labels += d
            .texts
            .iter()
            .filter(|t| t.adapter.as_deref().is_some_and(|a| a != "none"))
            .count();
        for u in d.unresolved {
            *unresolved.entry(u).or_default() += 1;
        }
    }
    // Adapter coverage against the skin's REAL demand — the honest UI progress metric.
    let mut demand: Vec<String> = Vec::new();
    for n in &names {
        for t in layout_window(&skin, &skin.windows[*n], (0.0, 0.0), &|_| None).texts {
            if let Some(a) = t.adapter.filter(|a| a != "none" && !a.is_empty()) {
                if !demand.contains(&a) {
                    demand.push(a);
                }
            }
        }
    }
    let status = caer_protocol::status::PlayerStatus::default();
    let st = caer_render::adapters::AdapterState {
        player_name: "Lilillyn",
        status: &status,
        target: Some(("giant skeleton", 45)),
        zone: Some("Cornwall"),
        fps: 60,
        // Layout sweep, not a live session — no sheet.
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
    let (bound, total) = caer_render::adapters::adapter_coverage(&st, &demand);

    println!(
        "=== layout sweep: skin `{skin_name}`, {} windows ===",
        names.len()
    );
    println!("  quads produced        : {quads}");
    println!("  text controls         : {texts}  ({adapter_labels} adapter-driven)");
    println!("  unresolved templates  : {} distinct", unresolved.len());
    println!("  adapters bound        : {bound}/{total} distinct  <- each unbound one is a game system still to port");
    let mut u: Vec<_> = unresolved.into_iter().collect();
    u.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (name, n) in u.iter().take(10) {
        println!("    {n:4}x  {name}");
    }
}

/// Composite one window onto a transparent canvas and write raw RGBA8.
fn render_one(skin: &Skin, ui_dir: &std::path::Path, window: &str, out_path: &str) {
    let Some(win) = skin.windows.get(window) else {
        eprintln!("uilayout: no window `{window}`. try: {:?}", {
            let mut n: Vec<&String> = skin.windows.keys().take(12).collect();
            n.sort();
            n
        });
        std::process::exit(1);
    };
    // A little margin so a frame drawn at the very edge is still visibly complete.
    const M: f32 = 8.0;
    let (w, h) = (
        (win.width as f32 + M * 2.0) as u32,
        (win.height as f32 + M * 2.0) as u32,
    );
    let draw = layout_window(skin, win, (M, M), &|a| Some(format!("<{a}>")));

    // Load only the pages this window actually touches.
    let mut pages: HashMap<String, TgaImage> = HashMap::new();
    for q in &draw.quads {
        if pages.contains_key(&q.texture) {
            continue;
        }
        let Some(tex) = skin.texture(&q.texture) else {
            eprintln!("  no texture page `{}`", q.texture);
            continue;
        };
        let path = ui_dir.join(&tex.file);
        match std::fs::read(&path).map(|b| caer_assets::tga::decode(&b)) {
            Ok(Ok(img)) => {
                pages.insert(q.texture.clone(), img);
            }
            _ => eprintln!("  could not load {}", path.display()),
        }
    }

    let mut canvas = vec![0u8; (w * h * 4) as usize];
    for q in &draw.quads {
        let Some(page) = pages.get(&q.texture) else {
            continue;
        };
        blit(&mut canvas, w, h, q, page);
    }

    std::fs::write(out_path, &canvas).expect("write raw");
    println!(
        "{out_path} {w}x{h}  ({} quads, {} texts, {} pages)",
        draw.quads.len(),
        draw.texts.len(),
        pages.len()
    );
    for t in &draw.texts {
        println!(
            "  text @{:.0},{:.0} font {:?}: {:?}",
            t.rect.x,
            t.rect.y,
            t.font.as_deref().unwrap_or("-"),
            t.text
        );
    }
    if !draw.unresolved.is_empty() {
        println!("  unresolved: {:?}", draw.unresolved);
    }
}

/// Nearest-neighbour stretch-blit with source-over alpha.
///
/// Nearest rather than bilinear on purpose: this exists to verify GEOMETRY, and a filtered result
/// would blur exactly the one-pixel seams a nine-slice bug produces.
fn blit(canvas: &mut [u8], cw: u32, ch: u32, q: &UiQuad, page: &TgaImage) {
    let (dw, dh) = (q.dst.w.round() as i32, q.dst.h.round() as i32);
    for dy in 0..dh {
        let py = q.dst.y.round() as i32 + dy;
        if py < 0 || py >= ch as i32 {
            continue;
        }
        // Map destination pixel back into the source band.
        let sy = q.src.y + (dy as f32 + 0.5) / dh as f32 * q.src.h;
        for dx in 0..dw {
            let px = q.dst.x.round() as i32 + dx;
            if px < 0 || px >= cw as i32 {
                continue;
            }
            let sx = q.src.x + (dx as f32 + 0.5) / dw as f32 * q.src.w;
            let Some(s) = page.pixel(sx as u32, sy as u32) else {
                continue;
            };
            let o = ((py as u32 * cw + px as u32) * 4) as usize;
            let a = f32::from(s[3]) / 255.0;
            for c in 0..3 {
                let dstc = f32::from(canvas[o + c]);
                canvas[o + c] = (f32::from(s[c]) * a + dstc * (1.0 - a)) as u8;
            }
            canvas[o + 3] = canvas[o + 3].max(s[3]);
        }
    }
}
