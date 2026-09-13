//! `shademap.pcx` — per-zone 8-bit terrain AO / baked shade (256×256, same lattice as height).

use std::io;

use caer_assets::pcx;

/// Decoded shade map: `data[y * width + x]` is 0..=255 (brighter = more lit).
#[derive(Debug, Clone)]
pub struct ShadeMap {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl ShadeMap {
    #[inline]
    #[must_use]
    pub fn get(&self, x: usize, y: usize) -> u8 {
        let x = x.min(self.width.saturating_sub(1));
        let y = y.min(self.height.saturating_sub(1));
        self.data[y * self.width + x]
    }

    /// Multiplier in 0..=1 for vertex colour / lighting (authored byte / 255).
    #[inline]
    #[must_use]
    pub fn factor(&self, x: usize, y: usize) -> f32 {
        f32::from(self.get(x, y)) / 255.0
    }

    /// Bilinear factor at continuous sample-grid UV in 0..width / 0..height.
    #[must_use]
    pub fn factor_at(&self, u: f32, v: f32) -> f32 {
        if self.width == 0 || self.height == 0 {
            return 1.0;
        }
        let max_x = (self.width - 1) as f32;
        let max_y = (self.height - 1) as f32;
        let x = u.clamp(0.0, max_x);
        let y = v.clamp(0.0, max_y);
        let x0 = x.floor() as usize;
        let y0 = y.floor() as usize;
        let x1 = (x0 + 1).min(self.width - 1);
        let y1 = (y0 + 1).min(self.height - 1);
        let tx = x - x0 as f32;
        let ty = y - y0 as f32;
        let s00 = self.factor(x0, y0);
        let s10 = self.factor(x1, y0);
        let s01 = self.factor(x0, y1);
        let s11 = self.factor(x1, y1);
        let a = s00 + (s10 - s00) * tx;
        let b = s01 + (s11 - s01) * tx;
        a + (b - a) * ty
    }
}

/// Decode `shademap.pcx` bytes.
pub fn load_shademap(pcx_bytes: &[u8]) -> io::Result<ShadeMap> {
    let p = pcx::decode8(pcx_bytes)?;
    Ok(ShadeMap {
        width: p.width,
        height: p.height,
        data: p.data,
    })
}

/// Pull `shademap.pcx` from a zone `datNNN.mpk`.
pub fn shademap_from_dat_mpk(dat_mpk: &[u8]) -> io::Result<Option<ShadeMap>> {
    let members = caer_assets::read(dat_mpk)?;
    let Some(entry) = members
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case("shademap.pcx"))
    else {
        return Ok(None);
    };
    load_shademap(&entry.data).map(Some)
}

/// Pull `water.pcx` (ocean/land mask) from a zone dat archive.
pub fn water_mask_from_dat_mpk(dat_mpk: &[u8]) -> io::Result<Option<ShadeMap>> {
    let members = caer_assets::read(dat_mpk)?;
    let Some(entry) = members
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case("water.pcx"))
    else {
        return Ok(None);
    };
    // Reuse ShadeMap as a generic 8-bit grid; water.pcx uses 0 for low/sea cells on coasts.
    load_shademap(&entry.data).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_and_samples() {
        // Use a real zone shademap when CAER_CLIENT is present; otherwise skip the PCX round-trip.
        let root = crate::terrain::client_root();
        let dat = root.join("zones/zone001/dat001.mpk");
        if !dat.is_file() {
            eprintln!("skip shademap decode: {}", dat.display());
            return;
        }
        let bytes = std::fs::read(&dat).unwrap();
        let m = shademap_from_dat_mpk(&bytes)
            .unwrap()
            .expect("zone001 has shademap");
        assert_eq!(m.width, 256);
        assert_eq!(m.height, 256);
        assert!(m.get(0, 0) > 0 || m.get(128, 128) > 0);
        let f = m.factor_at(10.5, 10.5);
        assert!((0.0..=1.0).contains(&f));
    }
}
