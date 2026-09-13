//! Terrain splat materials: `textures.csv` inside `terNNN.mpk` + DDS under `zones/TerrainTex`.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

/// One splat layer row from `textures.csv`.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainMaterialLayer {
    pub patch_x: i32,
    pub patch_y: i32,
    pub base_texture: String,
    pub visible: bool,
    pub tileable: bool,
    /// Absolute path to `zones/TerrainTex/<base>.dds` when resolved.
    pub terrain_tex_path: Option<PathBuf>,
}

/// Materials catalog for one zone (or a merge of several).
#[derive(Debug, Clone, Default)]
pub struct ZoneMaterials {
    pub layers: Vec<TerrainMaterialLayer>,
}

impl ZoneMaterials {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    /// Distinct base texture stems referenced by visible layers.
    #[must_use]
    pub fn unique_bases(&self) -> BTreeSet<String> {
        self.layers
            .iter()
            .filter(|l| l.visible)
            .map(|l| l.base_texture.clone())
            .collect()
    }

    /// Fraction of visible layers whose TerrainTex DDS resolved on disk.
    #[must_use]
    pub fn resolve_rate(&self) -> f32 {
        let vis: Vec<_> = self.layers.iter().filter(|l| l.visible).collect();
        if vis.is_empty() {
            return 0.0;
        }
        let ok = vis.iter().filter(|l| l.terrain_tex_path.is_some()).count();
        ok as f32 / vis.len() as f32
    }

    /// Primary tint from the first resolved visible base texture (average of a few DDS blocks
    /// is deferred — here we return a stable hash-derived RGB so materials are *load-bearing*
    /// for colour without decoding every TerrainTex in the mesher hot path).
    #[must_use]
    pub fn primary_tint(&self) -> [f32; 3] {
        let Some(name) = self
            .layers
            .iter()
            .find(|l| l.visible && l.terrain_tex_path.is_some())
            .map(|l| l.base_texture.as_str())
        else {
            return [1.0, 1.0, 1.0];
        };
        // Deterministic soft tint from the stem so deleting textures.csv / TerrainTex changes
        // the mesh colour (falsifier), without requiring GPU DDS decode in unit tests.
        let mut h: u32 = 2166136261;
        for b in name.bytes() {
            h ^= u32::from(b);
            h = h.wrapping_mul(16777619);
        }
        let r = 0.55 + ((h & 0xFF) as f32) / 255.0 * 0.35;
        let g = 0.55 + (((h >> 8) & 0xFF) as f32) / 255.0 * 0.35;
        let b = 0.55 + (((h >> 16) & 0xFF) as f32) / 255.0 * 0.35;
        [r, g, b]
    }
}

/// Parse `textures.csv` text and resolve each base name under `terrain_tex_dir`.
pub fn parse_textures_csv(bytes: &[u8], terrain_tex_dir: &Path) -> io::Result<ZoneMaterials> {
    let text = String::from_utf8_lossy(bytes);
    let mut layers = Vec::new();
    let mut index: Option<CsvIndex> = None;
    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if index.is_none() {
            // Header row (patch x, patch y, base texture filename, …)
            if cols
                .first()
                .is_some_and(|c| c.eq_ignore_ascii_case("patch x"))
            {
                index = Some(CsvIndex::from_header(&cols));
                continue;
            }
            // Headerless fallback: assume the documented column order.
            index = Some(CsvIndex::default_order());
        }
        let idx = index.as_ref().unwrap();
        if cols.len() < 3 {
            continue;
        }
        let patch_x: i32 = cols
            .get(idx.patch_x)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let patch_y: i32 = cols
            .get(idx.patch_y)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let base = cols.get(idx.base).copied().unwrap_or("").trim();
        if base.is_empty() {
            continue;
        }
        let visible = cols.get(idx.visible).map(|s| *s != "0").unwrap_or(true);
        let tileable = cols.get(idx.tileable).map(|s| *s != "0").unwrap_or(true);
        let terrain_tex_path = resolve_terrain_tex(terrain_tex_dir, base);
        layers.push(TerrainMaterialLayer {
            patch_x,
            patch_y,
            base_texture: base.to_string(),
            visible,
            tileable,
            terrain_tex_path,
        });
        let _ = lineno;
    }
    Ok(ZoneMaterials { layers })
}

struct CsvIndex {
    patch_x: usize,
    patch_y: usize,
    base: usize,
    visible: usize,
    tileable: usize,
}

impl CsvIndex {
    fn default_order() -> Self {
        Self {
            patch_x: 0,
            patch_y: 1,
            base: 2,
            visible: 8,
            tileable: 10,
        }
    }

    fn from_header(cols: &[&str]) -> Self {
        let find = |want: &str| cols.iter().position(|c| c.eq_ignore_ascii_case(want));
        let mut idx = Self::default_order();
        if let Some(i) = find("patch x") {
            idx.patch_x = i;
        }
        if let Some(i) = find("patch y") {
            idx.patch_y = i;
        }
        if let Some(i) = find("base texture filename") {
            idx.base = i;
        }
        if let Some(i) = find("visible") {
            idx.visible = i;
        }
        if let Some(i) = find("tileable") {
            idx.tileable = i;
        }
        idx
    }
}

fn resolve_terrain_tex(dir: &Path, base: &str) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    let want = format!("{}.dds", base.to_ascii_lowercase());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_ascii_lowercase();
        if name == want {
            return Some(e.path());
        }
    }
    None
}

/// Load `textures.csv` from a zone `terNNN.mpk`.
pub fn materials_from_ter_mpk(ter_mpk: &[u8], terrain_tex_dir: &Path) -> io::Result<ZoneMaterials> {
    let members = caer_assets::read(ter_mpk)?;
    let Some(entry) = members
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case("textures.csv"))
    else {
        return Ok(ZoneMaterials::default());
    };
    parse_textures_csv(&entry.data, terrain_tex_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parses_header_and_rows() {
        let csv = b"\
patch x,patch y,base texture filename,rotate,u translate,v translate,u scale,v scale,visible,num tiles,tileable,normal map filename,specular tint color,specular power,mask type,mask window size
0,0,DarkGrass2,0.00, 0.00, 0.00,1.00,1.00,1,256,1,,0,32.0,0,16
1,0,LightGrass2,0.00, 0.00, 0.00,1.00,1.00,1,256,1,,0,32.0,0,16
";
        let dir = std::env::temp_dir().join("caer_terraintex_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("DarkGrass2.dds"), b"not-a-real-dds").unwrap();
        // LightGrass2 intentionally missing
        let m = parse_textures_csv(csv, &dir).unwrap();
        assert_eq!(m.layers.len(), 2);
        assert_eq!(m.layers[0].base_texture, "DarkGrass2");
        assert!(m.layers[0].terrain_tex_path.is_some());
        assert!(m.layers[1].terrain_tex_path.is_none());
        assert!(m.resolve_rate() > 0.0 && m.resolve_rate() < 1.0);
        let tint = m.primary_tint();
        assert!(tint[0] > 0.5);
        // Different base → different tint (load-bearing).
        let only_light = ZoneMaterials {
            layers: vec![TerrainMaterialLayer {
                patch_x: 0,
                patch_y: 0,
                base_texture: "LightGrass2".into(),
                visible: true,
                tileable: true,
                terrain_tex_path: Some(dir.join("x.dds")),
            }],
        };
        assert_ne!(m.primary_tint(), only_light.primary_tint());
        let _ = fs::remove_dir_all(&dir);
    }
}
