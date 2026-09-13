//! A character's authored appearance — the persisted visual bytes DOL returns to every client.
//!
//! `CharacterCreateRequestHandler` reads `EyeSize`, `LipSize`, `EyeColor`, `HairColor`,
//! `FaceType`, `HairStyle`, and `MoodType` off the create packet and writes them onto the
//! character row; the overview and in-world packets hand them back out again. They are
//! **character state**, not a render setting: two Firbolg males differ by nothing else, and a
//! client that ignores them draws every player of a race as the same person.
//!
//! The five discrete fields and the two packed morph fields have different default semantics. A
//! create packet with `CustomMode == 0` leaves the source head unblended; an explicitly customised
//! head uses the source UI's neutral morph tick (`4`) unless the player moves a slider. The overview
//! carries `CustomisationStep`, which preserves that distinction. [`AvatarAppearance`] keeps it
//! through rendering so a historical all-zero character is not mistaken for “all sliders left”.

/// The four stock `character_customize.xml` morph slots, in the source table's display order.
///
/// DOLSharp/SoloDAoC's `PacketLib1124` labels `EyeSize` as low nibble **Eye** / high nibble
/// **Nose**, and `LipSize` as low nibble **Lip/Ear** / high nibble **Chin/Jaw**.  The retail
/// `fig3descriptions.csv` gives those slots race-specific names (for example Celt: Nose, Eyes,
/// Lips, Jaw; Firbolg: Nose, Eyes, Jaw Length, Ears).  We retain the byte-level owner here and
/// use the client table only for labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FacialMorphSlot {
    Nose,
    Eyes,
    LipsOrEars,
    JawOrChin,
}

impl FacialMorphSlot {
    /// The four source-form slots in `character_customize.xml` display/packet order.
    pub const ALL: [Self; 4] = [Self::Nose, Self::Eyes, Self::LipsOrEars, Self::JawOrChin];

    /// Convert the retail form's zero-based `morph_N` / slider slot to its packed protocol field.
    #[must_use]
    pub const fn from_source_slot(slot: u8) -> Option<Self> {
        match slot {
            0 => Some(Self::Nose),
            1 => Some(Self::Eyes),
            2 => Some(Self::LipsOrEars),
            3 => Some(Self::JawOrChin),
            _ => None,
        }
    }

    /// The zero-based retail form slot for this packed protocol field.
    #[must_use]
    pub const fn source_slot(self) -> u8 {
        match self {
            Self::Nose => 0,
            Self::Eyes => 1,
            Self::LipsOrEars => 2,
            Self::JawOrChin => 3,
        }
    }
}

/// `character_customize.xml` authors `Numticks=9`: nine selectable positions, zero through eight.
///
/// This is independently corroborated by the retail `figtofig3main.csv`: its neutral default is
/// `4` for every one of the four fields and its authored values span `0..=8`.  Treating the XML
/// attribute as an inclusive maximum would invent a tenth position which no source row uses.
pub const FACIAL_MORPH_MAX_TICK: u8 = 8;

/// The centre position of every stock nine-tick facial slider.
pub const FACIAL_MORPH_NEUTRAL_TICK: u8 = 4;

/// Read one source-order facial morph tick while preserving the other three packed values.
#[must_use]
pub fn facial_morph_tick(eye_size: u8, lip_size: u8, slot: FacialMorphSlot) -> u8 {
    match slot {
        FacialMorphSlot::Nose => (eye_size >> 4) & 0x0F,
        FacialMorphSlot::Eyes => eye_size & 0x0F,
        FacialMorphSlot::LipsOrEars => lip_size & 0x0F,
        FacialMorphSlot::JawOrChin => (lip_size >> 4) & 0x0F,
    }
}

/// Set one source-order facial morph tick without clobbering the other packed nibble.
#[must_use]
pub fn with_facial_morph_tick(
    eye_size: u8,
    lip_size: u8,
    slot: FacialMorphSlot,
    tick: u8,
) -> (u8, u8) {
    let tick = tick.min(FACIAL_MORPH_MAX_TICK);
    match slot {
        FacialMorphSlot::Nose => ((eye_size & 0x0F) | (tick << 4), lip_size),
        FacialMorphSlot::Eyes => ((eye_size & 0xF0) | tick, lip_size),
        FacialMorphSlot::LipsOrEars => (eye_size, (lip_size & 0xF0) | tick),
        FacialMorphSlot::JawOrChin => (eye_size, (lip_size & 0x0F) | (tick << 4)),
    }
}

/// The five appearance bytes, in the order the create packet carries them.
///
/// `Copy` + `Hash` on purpose: this is part of the assembled-avatar cache key, because two looks
/// on one race are two different meshes and a cache that cannot tell them apart hands back
/// whichever was built first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Customization {
    /// Low nibble is the skin tone, 1–4 on the wire. The high nibble carries the eye colour
    /// proper, which the body meshes do not use.
    pub eye_color: u8,
    /// Indexes the `nS` columns of `fig3haircolormap.csv`, which name a texture sheet — not a
    /// tint. Hair is textured, so this picks the sheet the mesh wears.
    pub hair_color: u8,
    /// Column of `fig3facemap.csv`: which face file this race+gender wears, 1..=7.
    pub face_type: u8,
    /// Which hairstyle, 1..=32. A style names a MESH and a SHEET together, so it has to be chosen
    /// before either is resolved.
    pub hair_style: u8,
    /// The face's expression row. Carried for wire fidelity; no mesh reads it yet.
    pub mood_type: u8,
}

impl Customization {
    /// Was anything customised at all? A false answer means the render is free to take every
    /// shipped default and the cache is free to reuse the race's base mesh.
    #[must_use]
    pub fn is_default(self) -> bool {
        self == Self::default()
    }
}

/// All visual state required to assemble one fig3 avatar.
///
/// [`Customization`] intentionally remains the five discrete asset selectors. This companion type
/// adds the two packed facial-morph bytes and whether the client should interpret them as authored
/// slider values. It belongs in the renderer cache key: a different nose or jaw is different
/// vertex geometry, not merely a shader parameter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct AvatarAppearance {
    pub customization: Customization,
    /// Packed Nose (high nibble) and Eyes (low nibble) source slider positions.
    pub eye_size: u8,
    /// Packed Lips/Ears (low nibble) and Jaw/Chin (high nibble) source slider positions.
    pub lip_size: u8,
    /// False means the character predates/declined the explicit customizer path, so the retail
    /// base head is left at its neutral shape even though the stored bytes are zero.
    pub facial_morphs_enabled: bool,
}

impl AvatarAppearance {
    /// The displayed source slider position for one facial feature.
    #[must_use]
    pub fn facial_morph_tick(self, slot: FacialMorphSlot) -> u8 {
        if self.facial_morphs_enabled {
            facial_morph_tick(self.eye_size, self.lip_size, slot).min(FACIAL_MORPH_MAX_TICK)
        } else {
            FACIAL_MORPH_NEUTRAL_TICK
        }
    }

    /// True only when the avatar needs no discrete or vertex-level appearance override.
    #[must_use]
    pub fn is_default(self) -> bool {
        self.customization.is_default() && !self.facial_morphs_enabled
    }
}

impl From<Customization> for AvatarAppearance {
    fn from(customization: Customization) -> Self {
        Self {
            customization,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn morph_slots_preserve_their_neighbouring_nibbles() {
        let (eyes, lips) = with_facial_morph_tick(0, 0, FacialMorphSlot::Nose, 8);
        let (eyes, lips) = with_facial_morph_tick(eyes, lips, FacialMorphSlot::Eyes, 3);
        let (eyes, lips) = with_facial_morph_tick(eyes, lips, FacialMorphSlot::LipsOrEars, 7);
        let (eyes, lips) = with_facial_morph_tick(eyes, lips, FacialMorphSlot::JawOrChin, 4);
        assert_eq!(eyes, 0x83);
        assert_eq!(lips, 0x47);
        assert_eq!(facial_morph_tick(eyes, lips, FacialMorphSlot::Nose), 8);
        assert_eq!(facial_morph_tick(eyes, lips, FacialMorphSlot::Eyes), 3);
        assert_eq!(
            facial_morph_tick(eyes, lips, FacialMorphSlot::LipsOrEars),
            7
        );
        assert_eq!(facial_morph_tick(eyes, lips, FacialMorphSlot::JawOrChin), 4);
    }

    #[test]
    fn morph_tick_cannot_escape_the_authored_slider_range() {
        let (eye_size, lip_size) =
            with_facial_morph_tick(0, 0, FacialMorphSlot::Nose, FACIAL_MORPH_MAX_TICK + 1);
        assert_eq!(eye_size, 0x80);
        assert_eq!(lip_size, 0);
    }

    #[test]
    fn historical_default_and_explicit_leftmost_morph_are_distinct() {
        let default_head = AvatarAppearance::default();
        assert_eq!(
            default_head.facial_morph_tick(FacialMorphSlot::Nose),
            FACIAL_MORPH_NEUTRAL_TICK
        );

        let explicit_left = AvatarAppearance {
            facial_morphs_enabled: true,
            ..AvatarAppearance::default()
        };
        assert_eq!(
            explicit_left.facial_morph_tick(FacialMorphSlot::Nose),
            0,
            "CustomisationStep/custom mode makes wire zero an authored slider position"
        );
    }
}
