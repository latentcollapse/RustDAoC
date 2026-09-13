//! Shared helpers for the pre-world harnesses.
//!
//! These six pieces were copy-pasted across every harness as each one was written to chase a
//! different defect. One definition each, so a change to the client-root rule or the legal-race
//! enumeration cannot drift between the tools that depend on it.

#![allow(dead_code)] // each harness uses a subset

use std::path::PathBuf;

/// The retail tree, or a hard failure.
///
/// REQ-025: a test that cannot run must not report pass, so this asserts rather than skipping.
pub fn root() -> PathBuf {
    let r = caer_assets::client_dep::required_caer_client_root("preworld integration test");
    assert!(
        r.join("figures").is_dir(),
        "CAER_CLIENT has no figures directory: {} — REQ-025: a test that cannot run must not \
         report pass.",
        r.display()
    );
    r
}

/// Every race/gender the create form offers, as `(eRace, label, wire gender)`.
///
/// Minotaur ships three realm-specific race ids behind one control, so each is its own body.
pub fn legal_combinations() -> Vec<(u8, &'static str, u8)> {
    let mut out: Vec<(u8, &'static str, u8)> = Vec::new();
    for realm in 1..=3u8 {
        for a in caer_protocol::creation_adapters::race_adapters(realm) {
            for wire in 0..=1u8 {
                if wire == 1 && a.male_only {
                    continue;
                }
                if out.iter().any(|(r, _, g)| *r == a.race_id && *g == wire) {
                    continue;
                }
                out.push((a.race_id, a.label, wire));
            }
        }
    }
    out
}

/// Fraction of pixels that are not the cleared background — ~0 means nothing drew.
pub fn coverage(rgba: &[u8]) -> f64 {
    let lit = rgba
        .chunks_exact(4)
        .filter(|p| p[0] > 12 || p[1] > 12 || p[2] > 12)
        .count();
    lit as f64 / (rgba.len() / 4).max(1) as f64
}

/// Copy one rendered cell into a contact sheet at cell index `i`.
pub fn blit_cell(sheet: &mut [u8], sheet_w: u32, cols: u32, cell: &[u8], cw: u32, ch: u32, i: u32) {
    let (cx, cy) = ((i % cols) * cw, (i / cols) * ch);
    for y in 0..ch {
        let src = (y * cw * 4) as usize;
        let dst = (((cy + y) * sheet_w + cx) * 4) as usize;
        let (Some(s), Some(d)) = (
            cell.get(src..src + (cw * 4) as usize),
            sheet.get_mut(dst..dst + (cw * 4) as usize),
        ) else {
            continue;
        };
        d.copy_from_slice(s);
    }
}

/// Write an RGBA8 buffer as a PNG, creating the parent directory.
pub fn write_png(path: &str, w: u32, h: u32, rgba: &[u8]) {
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(file) = std::fs::File::create(path) else {
        return;
    };
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    if let Ok(mut writer) = enc.write_header() {
        let _ = writer.write_image_data(rgba);
    }
}

/// The idle palette for a model, or `None` when it carries no rig.
pub fn idle_palette(
    models: &caer_render::entities::EntityModels,
    model: u16,
    t: f32,
) -> Option<Vec<[[f32; 4]; 4]>> {
    let rig = models.skinned_rig(model)?;
    let p = caer_render::anim_skin::build_palettes(
        rig,
        &[caer_render::anim_skin::UniquePaletteJob {
            loco: caer_render::entities::Loco::Idle,
            t,
            blend: None,
        }],
    );
    (!p.is_empty()).then_some(p)
}
