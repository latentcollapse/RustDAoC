//! The player's own vitals — health / power / endurance / concentration.
//!
//! Source: `CharacterStatusUpdate` (0xAD), the server's authoritative readout of *our* character.
//! This is what the HUD bars are made of, and it is also the only place the client learns its own
//! current/max health (entity updates carry percentages for *other* livings, not our absolute HP).
//!
//! ## Why this one is trustworthy (unlike the Phase-B combat codes)
//! 0xAD is **server→client**, and the server is the oracle: `PacketLib190.SendStatusUpdate` writes
//! these exact fields in this exact order, and DoL-lineage servers are what we target. That makes
//! the oracle authoritative *by construction* here — the opposite of `0x74`/`0xB0`, which are
//! client→server codes where the oracle only documents an enum it never verified.
//!
//! It is additionally pinned to real bytes: 0xAD appears in **every** golden capture (29–349
//! occurrences each), always 22 bytes, and the percentage bytes agree with the current/max shorts
//! to the rounding (e.g. `hp% = 92` alongside `1848/2004` → 92.2). Both halves of the packet are
//! therefore cross-checking each other; see the tests.
//!
//! ## Layout (22 bytes, big-endian shorts per the crate-wide convention)
//! ```text
//! [health% u8][mana% u8][sitting u8][endurance% u8][concentration% u8][unknown u8]
//! [max_mana u16][max_endurance u16][max_concentration u16][max_health u16]
//! [health u16][endurance u16][mana u16][concentration u16]
//! ```
//! Note the tail's asymmetry: the *max* block is ordered mana/end/conc/health, while the *current*
//! block is health/end/mana/conc. That is not a transcription slip — it is what the oracle writes,
//! and swapping them makes the percentages disagree with the values (the tests would catch it).

use crate::codec::PacketReader;
use crate::error::Result;

/// The player's vitals as of the last `CharacterStatusUpdate` (0xAD).
///
/// Percentages arrive from the server directly rather than being derived here: the server rounds
/// them its own way, and the HUD should show the same number the server believes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerStatus {
    pub health_pct: u8,
    pub mana_pct: u8,
    pub endurance_pct: u8,
    pub concentration_pct: u8,
    /// Non-zero while the character is sitting (the server's own sit flag, echoed back to us).
    pub sitting: bool,
    pub health: u16,
    pub max_health: u16,
    pub mana: u16,
    pub max_mana: u16,
    pub endurance: u16,
    pub max_endurance: u16,
    pub concentration: u16,
    pub max_concentration: u16,
}

impl Default for PlayerStatus {
    /// A full-health placeholder for before the first 0xAD arrives.
    ///
    /// Deliberately NOT all-zeroes: zero would render as an empty health bar and read as "you are
    /// dead" during the second or two between world entry and the first status packet. Full bars
    /// are the honest guess — the server only spawns us alive.
    fn default() -> Self {
        Self {
            health_pct: 100,
            mana_pct: 100,
            endurance_pct: 100,
            concentration_pct: 100,
            sitting: false,
            health: 0,
            max_health: 0,
            mana: 0,
            max_mana: 0,
            endurance: 0,
            max_endurance: 0,
            concentration: 0,
            max_concentration: 0,
        }
    }
}

impl PlayerStatus {
    /// Health as a 0.0–1.0 fraction for a bar widget.
    ///
    /// Prefers the absolute values (finer than the server's integer percent) and falls back to the
    /// percent byte when the maxima are still unknown — which is exactly the [`Default`] case.
    #[must_use]
    pub fn health_frac(&self) -> f32 {
        frac(self.health, self.max_health, self.health_pct)
    }

    #[must_use]
    pub fn mana_frac(&self) -> f32 {
        frac(self.mana, self.max_mana, self.mana_pct)
    }

    #[must_use]
    pub fn endurance_frac(&self) -> f32 {
        frac(self.endurance, self.max_endurance, self.endurance_pct)
    }

    #[must_use]
    pub fn concentration_frac(&self) -> f32 {
        frac(
            self.concentration,
            self.max_concentration,
            self.concentration_pct,
        )
    }

    /// Whether this character has a power pool at all. Pure melee classes (Armsman, Mercenary…)
    /// report `max_mana = 0`, and the HUD hides the power bar rather than drawing a permanent
    /// empty one.
    #[must_use]
    pub fn has_power(&self) -> bool {
        self.max_mana > 0
    }
}

/// Shared bar-fraction rule: absolute values when we have them, the server's percent otherwise.
fn frac(cur: u16, max: u16, pct: u8) -> f32 {
    if max > 0 {
        f32::from(cur) / f32::from(max)
    } else {
        f32::from(pct) / 100.0
    }
    .clamp(0.0, 1.0)
}

/// Decode a `CharacterStatusUpdate` (0xAD) body.
pub fn decode_status_update(payload: &[u8]) -> Result<PlayerStatus> {
    let mut r = PacketReader::new(payload);
    let health_pct = r.u8()?;
    let mana_pct = r.u8()?;
    let sitting = r.u8()? != 0;
    let endurance_pct = r.u8()?;
    let concentration_pct = r.u8()?;
    r.u8()?; // unknown — 0 in every captured sample; the oracle comments hint at a dead flag

    let max_mana = r.u16()?;
    let max_endurance = r.u16()?;
    let max_concentration = r.u16()?;
    let max_health = r.u16()?;
    let health = r.u16()?;
    let endurance = r.u16()?;
    let mana = r.u16()?;
    let concentration = r.u16()?;

    Ok(PlayerStatus {
        health_pct,
        mana_pct,
        endurance_pct,
        concentration_pct,
        sitting,
        health,
        max_health,
        mana,
        max_mana,
        endurance,
        max_endurance,
        concentration,
        max_concentration,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real bytes, `captures/cap_20260716_211047_001.jsonl` — a character at full everything.
    const FULL: &str = "64640064640001ec0073017a07380738007301ec017a";

    /// Real bytes, `captures/cap_20260714_230416_conn38614.jsonl` — damaged, 1848/2004 HP (92%).
    const HURT: &str = "5c640064640001ec0073017a07d40738007301ec017a";

    /// Real bytes, `captures/cap_20260714_230416_conn41042.jsonl` — endurance drained, 95/100.
    const TIRED: &str = "6464005f640000cf006400a7028d028d005f00cf00a7";

    /// Real bytes, same capture — the sitting flag set. Note it is **2, not 1**, which is why
    /// the field is decoded as "non-zero" rather than compared against 1.
    const SITTING: &str = "6464026464000119007300c0035003500073011900c0";

    fn hex(s: &str) -> Vec<u8> {
        let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..clean.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&clean[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn decodes_a_full_health_capture_byte_exactly() {
        let s = decode_status_update(&hex(FULL)).expect("decode");
        assert_eq!(s.health_pct, 100);
        assert_eq!(s.mana_pct, 100);
        assert_eq!(s.endurance_pct, 100);
        assert_eq!(s.concentration_pct, 100);
        assert!(!s.sitting);
        // Big-endian shorts. Little-endian would read max_mana as 60417, which is the cheapest
        // possible tell that the byte order is wrong.
        assert_eq!(s.max_mana, 492);
        assert_eq!(s.max_endurance, 115);
        assert_eq!(s.max_concentration, 378);
        assert_eq!(s.max_health, 1848);
        assert_eq!(s.health, 1848);
        assert_eq!(s.endurance, 115);
        assert_eq!(s.mana, 492);
        assert_eq!(s.concentration, 378);
    }

    #[test]
    fn percentages_agree_with_the_absolute_values() {
        // The load-bearing test: the percent bytes and the current/max shorts are independent
        // encodings of the same fact, so agreement proves the field ORDER is right — including
        // the max-block/current-block asymmetry that a "tidier" reordering would break.
        for sample in [FULL, HURT, TIRED, SITTING] {
            let s = decode_status_update(&hex(sample)).expect("decode");
            for (cur, max, pct, what) in [
                (s.health, s.max_health, s.health_pct, "health"),
                (s.mana, s.max_mana, s.mana_pct, "mana"),
                (s.endurance, s.max_endurance, s.endurance_pct, "endurance"),
                (
                    s.concentration,
                    s.max_concentration,
                    s.concentration_pct,
                    "concentration",
                ),
            ] {
                assert!(max > 0, "{what}: max should be populated in these samples");
                assert!(cur <= max, "{what}: current {cur} exceeds max {max}");
                let derived = (u32::from(cur) * 100 / u32::from(max)) as u8;
                assert_eq!(
                    derived, pct,
                    "{what}: percent byte {pct} disagrees with {cur}/{max}"
                );
            }
        }
    }

    #[test]
    fn hurt_capture_reads_as_damaged() {
        let s = decode_status_update(&hex(HURT)).expect("decode");
        assert_eq!(s.health, 1848);
        assert_eq!(s.max_health, 2004);
        assert_eq!(s.health_pct, 92);
        assert!(
            (s.health_frac() - 0.922).abs() < 0.01,
            "got {}",
            s.health_frac()
        );
        // The bar must be driven by the absolute values, not the coarser percent byte.
        assert!(s.health_frac() > 0.92 && s.health_frac() < 0.93);
    }

    #[test]
    fn the_sitting_flag_is_any_nonzero_value() {
        // The captured sit flag is 0x02, so a `== 1` comparison would silently never fire.
        let bytes = hex(SITTING);
        assert_eq!(bytes[2], 0x02, "test vector should be the sitting sample");
        assert!(decode_status_update(&bytes).expect("decode").sitting);
        assert!(!decode_status_update(&hex(FULL)).expect("decode").sitting);
    }

    #[test]
    fn endurance_drain_is_visible() {
        let s = decode_status_update(&hex(TIRED)).expect("decode");
        assert_eq!(s.endurance, 95);
        assert_eq!(s.max_endurance, 100);
        assert!((s.endurance_frac() - 0.95).abs() < 1e-6);
        assert!(
            (s.health_frac() - 1.0).abs() < 1e-6,
            "health untouched in this sample"
        );
    }

    #[test]
    fn a_short_payload_errors_rather_than_panicking() {
        // The wire is untrusted: a truncated packet must not index past the end.
        for len in 0..22 {
            assert!(
                decode_status_update(&hex(FULL)[..len]).is_err(),
                "len {len} should not decode"
            );
        }
    }

    #[test]
    fn default_reads_as_alive_with_full_bars() {
        // Guards the "empty bars for the first second in the world" trap.
        let s = PlayerStatus::default();
        assert!((s.health_frac() - 1.0).abs() < 1e-6);
        assert!((s.endurance_frac() - 1.0).abs() < 1e-6);
        assert!(
            !s.has_power(),
            "no maxima known yet, so the power bar stays hidden"
        );
    }

    #[test]
    fn a_melee_class_has_no_power_bar() {
        let mut s = decode_status_update(&hex(FULL)).expect("decode");
        assert!(s.has_power());
        s.max_mana = 0;
        assert!(!s.has_power());
        // …and the fraction must not divide by zero.
        assert!(s.mana_frac().is_finite());
    }
}
