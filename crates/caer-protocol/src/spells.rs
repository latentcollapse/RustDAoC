//! Spell cast / effect / interrupt S2C packets (0x72 / 0x1B / 0x73).
//!
//! ## Provenance
//!
//! REQ-021 / SCN-07: decoded against SoloDAoC PacketLib senders. 1.127 inherits effect from
//! PacketLib174 (drops the PacketLib168 `0xFFBF` trailer). Cast and interrupt remain PacketLib168.
//! OWN_CAPTURE: `captures/cap_20260716_211047_001.bin` carries 0x72 and 0x1B; no lab capture yet
//! carries 0x73 (OPEN_ORACLE for interrupt).
//!
//! ## Wire form
//!
//! **SpellCastAnimation 0x72** (`PacketLib168.SendSpellCastAnimation`):
//! ```text
//!   u16 BE  caster object id
//!   u16 BE  spell id
//!   u16 BE  cast time
//!   u16 BE  0
//! ```
//!
//! **SpellEffectAnimation 0x1B** (`PacketLib174.SendSpellEffectAnimation` — 1.127):
//! ```text
//!   u16 BE  caster object id
//!   u16 BE  spell id
//!   u16 BE  target object id (0 if none)
//!   u16 BE  bolt time
//!   u8     no_sound (1 = mute)
//!   u8     success
//!   // NO 0xFFBF trailer (PacketLib168 had one; 174 dropped it)
//! ```
//!
//! **InterruptSpellCast 0x73** (`PacketLib168.SendInterruptAnimation`):
//! ```text
//!   u16 BE  living object id
//!   u16 BE  1
//! ```

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

/// Provenance tag for cast start (REQ-021 / SCN-07).
pub const CAST_PROVENANCE: &str = "SpellCastAnimation 0x72";
/// Provenance tag for effect (REQ-021 / SCN-07).
pub const EFFECT_PROVENANCE: &str = "SpellEffectAnimation 0x1B";
/// Provenance tag for interrupt (REQ-021 / SCN-07).
pub const INTERRUPT_PROVENANCE: &str = "InterruptSpellCast 0x73";

/// A decoded SpellCastAnimation (0x72).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpellCastAnimation {
    pub caster_id: u16,
    pub spell_id: u16,
    pub cast_time: u16,
}

impl SpellCastAnimation {
    #[must_use]
    pub fn provenance(self) -> &'static str {
        CAST_PROVENANCE
    }
}

/// A decoded SpellEffectAnimation (0x1B) — PacketLib174 / 1.127 layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpellEffectAnimation {
    pub caster_id: u16,
    pub spell_id: u16,
    pub target_id: u16,
    pub bolt_time: u16,
    pub no_sound: bool,
    pub success: u8,
}

impl SpellEffectAnimation {
    #[must_use]
    pub fn provenance(self) -> &'static str {
        EFFECT_PROVENANCE
    }

    /// Packet success byte: nonzero is a completed land; 0 is resist / failure.
    #[must_use]
    pub fn succeeded(self) -> bool {
        self.success != 0
    }
}

/// A decoded InterruptSpellCast (0x73).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterruptSpellCast {
    pub object_id: u16,
}

impl InterruptSpellCast {
    #[must_use]
    pub fn provenance(self) -> &'static str {
        INTERRUPT_PROVENANCE
    }
}

/// Decode SpellCastAnimation (PacketLib168).
pub fn decode_cast(payload: &[u8]) -> Result<SpellCastAnimation> {
    let mut r = PacketReader::new(payload);
    let caster_id = r.u16()?;
    let spell_id = r.u16()?;
    let cast_time = r.u16()?;
    let _pad = r.u16()?;
    Ok(SpellCastAnimation {
        caster_id,
        spell_id,
        cast_time,
    })
}

/// Encode matching `PacketLib168.SendSpellCastAnimation`.
#[must_use]
pub fn encode_cast(c: &SpellCastAnimation) -> Vec<u8> {
    let mut w = PacketWriter::with_capacity(8);
    w.u16(c.caster_id).u16(c.spell_id).u16(c.cast_time).u16(0);
    w.into_bytes()
}

/// Decode SpellEffectAnimation (PacketLib174 — no 0xFFBF trailer).
pub fn decode_effect(payload: &[u8]) -> Result<SpellEffectAnimation> {
    let mut r = PacketReader::new(payload);
    let caster_id = r.u16()?;
    let spell_id = r.u16()?;
    let target_id = r.u16()?;
    let bolt_time = r.u16()?;
    let no_sound = r.u8()? != 0;
    let success = r.u8()?;
    Ok(SpellEffectAnimation {
        caster_id,
        spell_id,
        target_id,
        bolt_time,
        no_sound,
        success,
    })
}

/// Encode matching `PacketLib174.SendSpellEffectAnimation` (1.127).
#[must_use]
pub fn encode_effect(e: &SpellEffectAnimation) -> Vec<u8> {
    let mut w = PacketWriter::with_capacity(10);
    w.u16(e.caster_id)
        .u16(e.spell_id)
        .u16(e.target_id)
        .u16(e.bolt_time)
        .u8(if e.no_sound { 1 } else { 0 })
        .u8(e.success);
    w.into_bytes()
}

/// Decode InterruptSpellCast (PacketLib168).
pub fn decode_interrupt(payload: &[u8]) -> Result<InterruptSpellCast> {
    let mut r = PacketReader::new(payload);
    let object_id = r.u16()?;
    let _one = r.u16()?;
    Ok(InterruptSpellCast { object_id })
}

/// Encode matching `PacketLib168.SendInterruptAnimation`.
#[must_use]
pub fn encode_interrupt(i: &InterruptSpellCast) -> Vec<u8> {
    let mut w = PacketWriter::with_capacity(4);
    w.u16(i.object_id).u16(1);
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cast_fields_match_oracle_and_own_capture() {
        let c = SpellCastAnimation {
            caster_id: 0x0030,
            spell_id: 407,
            cast_time: 0,
        };
        let body = encode_cast(&c);
        assert_eq!(body, [0x00, 0x30, 0x01, 0x97, 0x00, 0x00, 0x00, 0x00]);
        let got = decode_cast(&body).expect("0x72");
        assert_eq!(got, c);
        assert_eq!(got.provenance(), CAST_PROVENANCE);
    }

    #[test]
    fn effect_fields_match_packetlib174_own_capture() {
        // OWN_CAPTURE sample: 10-byte body, no 0xFFBF.
        let own = [0x00, 0x30, 0x01, 0x97, 0x01, 0x76, 0x00, 0x00, 0x00, 0x01];
        let got = decode_effect(&own).expect("0x1B OWN_CAPTURE");
        assert_eq!(
            got,
            SpellEffectAnimation {
                caster_id: 0x0030,
                spell_id: 407,
                target_id: 0x0176,
                bolt_time: 0,
                no_sound: false,
                success: 1,
            }
        );
        assert_eq!(encode_effect(&got), own);
        assert_eq!(got.provenance(), EFFECT_PROVENANCE);
    }

    #[test]
    fn interrupt_fields_match_oracle_sender() {
        let i = InterruptSpellCast { object_id: 0x0030 };
        let body = encode_interrupt(&i);
        assert_eq!(body, [0x00, 0x30, 0x00, 0x01]);
        let got = decode_interrupt(&body).expect("0x73");
        assert_eq!(got, i);
        assert_eq!(got.provenance(), INTERRUPT_PROVENANCE);
    }

    #[test]
    fn packetlib168_effect_trailer_is_rejected_as_short_read_ok_on_174() {
        // 174 decoder stops at 10 bytes — a 168 body with trailing 0xFFBF still decodes the
        // leading fields (extra bytes ignored by not reading them). Assert 10B is the encode size.
        let e = SpellEffectAnimation {
            caster_id: 1,
            spell_id: 2,
            target_id: 3,
            bolt_time: 4,
            no_sound: true,
            success: 0,
        };
        assert_eq!(encode_effect(&e).len(), 10);
    }
}
