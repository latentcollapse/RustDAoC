//! CharacterOverview (s2c 0xFC) decoder — the character-select screen's data.
//!
//! Mirror of the oracle's `PacketLib1126.SendCharacterOverview(eRealm)`: a 16-byte header
//! (four LE u32s, the first carrying the realm-switcher flag bits) followed by exactly ten
//! realm-local character slots. An empty slot is a single 0x00 byte; an occupied slot opens
//! with the character's level (nonzero) and carries name/location/class/race strings plus
//! appearance, equipment, and stat fields.
//!
//! Layout verified byte-for-byte against the golden trace's 172-byte overview (account
//! `rustdaoc`, char Lilillyn): header 16 + occupied slot 147 + nine empty slots 9 = 172.

use crate::codec::PacketReader;
use crate::error::Result;

/// Visible equipment models from the overview's 20 LE shorts (OPEN_ORACLE PacketLib1126).
///
/// These are `objects.csv` ids, the same numbers `0x15` carries. Character select has no
/// LivingEquipmentUpdate for the dummy on the stage — this *is* the dress packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OverviewGear {
    pub helmet: u16,
    pub gloves: u16,
    pub boots: u16,
    pub torso: u16,
    pub cloak: u16,
    pub legs: u16,
    pub arms: u16,
    pub righthand: u16,
    pub lefthand: u16,
    pub twohand: u16,
    pub ranged: u16,
    pub extension_torso: u8,
    pub extension_gloves: u8,
    pub extension_boots: u8,
}

impl OverviewGear {
    /// Project onto the same `EquipmentUpdate` the world avatar binder already understands.
    #[must_use]
    pub fn to_equipment_update(self, object_id: u16) -> crate::equipment::EquipmentUpdate {
        use crate::equipment::{slot, EquipmentUpdate, VisibleItem};
        let mut items = Vec::new();
        let push = |items: &mut Vec<VisibleItem>, s: u8, model: u16, ext: Option<u8>| {
            if model != 0 {
                items.push(VisibleItem {
                    slot: s,
                    model,
                    extension: ext,
                    texture: None,
                    effect: None,
                    new_emblem: false,
                });
            }
        };
        push(&mut items, slot::HELM, self.helmet, None);
        push(
            &mut items,
            slot::HANDS,
            self.gloves,
            Some(self.extension_gloves),
        );
        push(
            &mut items,
            slot::FEET,
            self.boots,
            Some(self.extension_boots),
        );
        push(
            &mut items,
            slot::TORSO,
            self.torso,
            Some(self.extension_torso),
        );
        push(&mut items, slot::CLOAK, self.cloak, None);
        push(&mut items, slot::LEGS, self.legs, None);
        push(&mut items, slot::ARMS, self.arms, None);
        push(&mut items, slot::RIGHTHAND, self.righthand, None);
        push(&mut items, slot::LEFTHAND, self.lefthand, None);
        push(&mut items, slot::TWOHAND, self.twohand, None);
        push(&mut items, slot::RANGED, self.ranged, None);
        EquipmentUpdate {
            object_id,
            items,
            ..EquipmentUpdate::default()
        }
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.helmet == 0
            && self.gloves == 0
            && self.boots == 0
            && self.torso == 0
            && self.cloak == 0
            && self.legs == 0
            && self.arms == 0
            && self.righthand == 0
            && self.lefthand == 0
            && self.twohand == 0
            && self.ranged == 0
    }
}

/// One occupied character slot on the overview screen.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CharacterSummary {
    /// Realm-local slot index (0..10) — the byte a CharacterSelectRequest (0x10) sends back.
    pub slot: u8,
    pub level: u8,
    pub name: String,
    /// Translated location description, e.g. "Camelot Hills".
    pub location: String,
    pub class_name: String,
    pub race_name: String,
    pub region: u8,
    /// Numeric class id (`eCharacterClass`).
    pub class_id: u8,
    /// 1 = Albion, 2 = Midgard, 3 = Hibernia.
    pub realm: u8,
    /// STR, QUI, CON, DEX, INT, PIE, EMP, CHA — the order the packet carries them.
    pub stats: [u8; 8],
    /// Packed race/gender byte from the overview (NOT the CharacterCreate layout).
    /// Decode with [`decode_overview_race_gender`].
    pub race_gender: u8,
    /// Worn models from the overview shorts. Empty gear still means "naked fig3".
    pub gear: OverviewGear,
    /// `EyeColor` low nibble is skin tone (1–4) on the create wire; overview uses the same byte.
    pub eye_color: u8,
    /// Packed Nose (high nibble) and Eyes (low nibble) values from the source character record.
    pub eye_size: u8,
    /// Packed Lips/Ears (low nibble) and Jaw/Chin (high nibble) values from the source record.
    pub lip_size: u8,
    /// DOL's persisted distinction between an untouched/automatic look and a player-authored
    /// customisation (`1` auto, `2` player-ended, `3` enabled). It disambiguates raw morph zero:
    /// zero can be a historical default or a deliberate leftmost slider position.
    pub customisation_step: u8,
    /// The whole authored look this character was created with, `eye_color` included.
    ///
    /// The overview is where the character-select screen gets its bodies, so dropping these bytes
    /// meant every saved character came back wearing the look tables' first row instead of the one
    /// their owner picked.
    pub custom: crate::customization::Customization,
}

impl CharacterSummary {
    /// All appearance bytes in the form the fig3 renderer needs.
    #[must_use]
    pub fn appearance(&self) -> crate::customization::AvatarAppearance {
        crate::customization::AvatarAppearance {
            customization: self.custom,
            eye_size: self.eye_size,
            lip_size: self.lip_size,
            facial_morphs_enabled: self.customisation_step != 0
                || self.eye_size != 0
                || self.lip_size != 0
                || !self.custom.is_default(),
        }
    }
}

/// The decoded character-select screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterOverview {
    /// Header flag bits (0x01/0x02 = realm-switcher allowed).
    pub flags: u32,
    /// Occupied slots only, in slot order.
    pub characters: Vec<CharacterSummary>,
}

/// Decode a v1126+ CharacterOverview payload (the packet body, header already stripped).
pub fn decode_character_overview(payload: &[u8]) -> Result<CharacterOverview> {
    let mut r = PacketReader::new(payload);

    let flags = r.u32_le()?;
    // Three reserved LE u32s (always written zero by the oracle).
    for _ in 0..3 {
        r.u32_le()?;
    }

    let mut characters = Vec::new();
    for slot in 0..10u8 {
        // An account with no characters at all gets ONLY the 16-byte header — the oracle
        // returns before writing any slot bytes (verified live: 16-byte 0xFC for caerbot).
        if r.remaining() == 0 {
            break;
        }
        let level = r.u8()?;
        if level == 0 {
            continue; // empty slot is the single zero byte
        }

        let name = r.pascal_string_int_le()?;
        r.u32_le()?; // constant 0x18 the oracle writes after the name
        r.u8()?; // "always 1"
        let eye_size = r.u8()?;
        let lip_size = r.u8()?;
        let eye_color = r.u8()?;
        let hair_color = r.u8()?;
        let face_type = r.u8()?;
        let hair_style = r.u8()?;
        let ext_boots_gloves = r.u8()?;
        let ext_torso_hood = r.u8()?;
        let customisation_step = r.u8()?;
        let mood_type = r.u8()?;
        r.bytes(13)?; // zero fill
        let location = r.pascal_string_int_le()?;
        let class_name = r.pascal_string_int_le()?;
        let race_name = r.pascal_string_int_le()?;
        r.u16_le()?; // current model
        let region = r.u8()?;
        r.u8()?; // region expansion
        let helmet = r.u16_le()?;
        let gloves = r.u16_le()?;
        let boots = r.u16_le()?;
        r.u16_le()?; // right-hand color
        let torso = r.u16_le()?;
        let cloak = r.u16_le()?;
        let legs = r.u16_le()?;
        let arms = r.u16_le()?;
        r.bytes(16)?; // eight color shorts
        let righthand = r.u16_le()?;
        let lefthand = r.u16_le()?;
        let twohand = r.u16_le()?;
        let ranged = r.u16_le()?;
        let gear = OverviewGear {
            helmet,
            gloves,
            boots,
            torso,
            cloak,
            legs,
            arms,
            righthand,
            lefthand,
            twohand,
            ranged,
            extension_torso: ext_torso_hood >> 4,
            extension_gloves: ext_boots_gloves & 0x0F,
            extension_boots: ext_boots_gloves >> 4,
        };
        let mut stats = [0u8; 8];
        for s in &mut stats {
            *s = r.u8()?;
        }
        let class_id = r.u8()?;
        let realm = r.u8()?;
        let race_gender = r.u8()?;
        r.bytes(2)?; // active-weapon-slot pair
        r.u8()?; // SI flag
        r.u8()?; // constitution (repeated)
        r.u8()?; // unknown trailing byte

        characters.push(CharacterSummary {
            slot,
            level,
            name,
            location,
            class_name,
            race_name,
            region,
            class_id,
            realm,
            stats,
            race_gender,
            gear,
            eye_color,
            eye_size,
            lip_size,
            customisation_step,
            custom: crate::customization::Customization {
                eye_color,
                hair_color,
                face_type,
                hair_style,
                mood_type,
            },
        });
    }

    Ok(CharacterOverview { flags, characters })
}

/// Decode the overview's packed race/gender byte (`PacketLib1126` write form).
///
/// Wire: `((Race & 0x10) << 2) + (Race & 0x0F) | (Gender << 4)` — **not** the CharacterCreate
/// `Race | (Gender << 7)` layout. Returns `(eRace, eGender)` where eGender is 0=male / 1=female.
#[must_use]
pub fn decode_overview_race_gender(packed: u8) -> (u8, u8) {
    let gender = (packed >> 4) & 0x0F;
    let race = (packed & 0x0F) | ((packed & 0x40) >> 2);
    (race, gender)
}

/// Convert overview/DB `eGender` (0 male / 1 female) to fig3 gender (1 male / 2 female).
#[must_use]
pub fn fig3_gender_from_db(e_gender: u8) -> u8 {
    e_gender.saturating_add(1).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact 172-byte overview payload from the golden trace (`cap_20260714_011449`,
    /// s2c 0xFC): one level-50 Albion character in slot 0, nine empty slots.
    const FULL_GOLDEN: &str = "\
        0100000000000000000000000000000032090000004c696c696c6c796e001800\
        00000144442001002000000200000000000000000000000000000e0000004361\
        6d656c6f742048696c6c73000800000050616c6164696e000b00000048696768\
        6c616e646572000ad9010000005500540000005100ba06520053000000000000\
        000000000000000000000003003b00000000005f327d323c5d3c3c0101130001\
        007d00000000000000000000";

    #[test]
    fn decodes_golden_overview() {
        let payload = full_golden();
        let ov = decode_character_overview(&payload).expect("golden overview must decode");
        assert_eq!(ov.flags, 1, "realm switcher flag from the capture");
        assert_eq!(ov.characters.len(), 1);
        let c = &ov.characters[0];
        assert_eq!(c.slot, 0);
        assert_eq!(c.level, 50);
        assert_eq!(c.name, "Lilillyn");
        assert_eq!(c.location, "Camelot Hills");
        assert_eq!(c.class_name, "Paladin");
        assert_eq!(c.race_name, "Highlander");
        assert_eq!(c.realm, 1);
        assert_eq!(c.region, 1);
        assert_eq!(c.stats, [95, 50, 125, 50, 60, 93, 60, 60]);
        assert_eq!(
            c.race_gender, 0x13,
            "Highlander female packed overview byte"
        );
        let (race, gender) = decode_overview_race_gender(c.race_gender);
        assert_eq!(race, 3, "Highlander");
        assert_eq!(gender, 1, "eGender female");
        assert_eq!(fig3_gender_from_db(gender), 2);
        assert!(
            !c.gear.is_empty(),
            "golden Paladin overview carries worn models"
        );
        assert_eq!(c.gear.gloves, 85);
        assert_eq!(c.gear.boots, 84);
        assert_eq!(c.gear.torso, 81);
        assert_eq!(c.gear.legs, 82);
        assert_eq!(c.gear.arms, 83);
        assert_eq!(c.gear.cloak, 1722);
    }

    #[test]
    fn decodes_empty_account_overview() {
        // An account with no characters gets the bare 16-byte header — exactly what the live
        // server sent for the fresh `caerbot` account.
        let mut payload = vec![0u8; 16];
        payload[0] = 0x01; // realm-switcher flag
        let ov = decode_character_overview(&payload).expect("bare-header overview must decode");
        assert_eq!(ov.flags, 1);
        assert!(ov.characters.is_empty());
    }

    #[test]
    fn decodes_empty_slots_overview() {
        // An account WITH characters always gets all ten slot bytes; all-empty is ten zeros.
        let mut payload = vec![0u8; 16];
        payload[0] = 0x01;
        payload.extend_from_slice(&[0u8; 10]);
        let ov = decode_character_overview(&payload).expect("empty-slots overview must decode");
        assert!(ov.characters.is_empty());
    }

    #[test]
    fn truncated_overview_errors_cleanly() {
        // Chop the golden payload mid-character: must be a ProtocolError, never a panic.
        let payload = full_golden();
        assert!(decode_character_overview(&payload[..40]).is_err());
    }

    fn full_golden() -> Vec<u8> {
        hex(FULL_GOLDEN)
    }

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
}
