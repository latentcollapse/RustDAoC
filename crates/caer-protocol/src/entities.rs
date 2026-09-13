//! Entity-stream decoders — how the bot SEES the world.
//!
//! During world entry (and afterward as things spawn) the server streams the visible-area
//! population: `NPCCreate` (0xDA) for living NPCs and `ObjectCreate` (0xD9) for static items.
//! Mirrors of the oracle's `PacketLib1124.SendNPCCreate` / `PacketLib176.SendObjectCreate`.
//!
//! Note the coordinate asymmetry: entity creates carry **integer** coordinates (u32 x/y,
//! u16 z, big-endian) while the player's own position packets (0x20/0xA9) use LE floats.
//! Both are real; don't unify them.

use crate::codec::PacketReader;
use crate::error::Result;

/// A living NPC announced by NPCCreate (0xDA). Layout verified against the golden trace
/// (111 NPCs around the Camelot Hills spawn decode clean).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Npc {
    pub object_id: u16,
    pub speed: u16,
    pub heading: u16,
    pub x: u32,
    pub y: u32,
    pub z: u16,
    pub model: u16,
    pub size: u8,
    /// Display level (high bit = statue, already masked off).
    pub level: u8,
    /// Realm in bits 6–7; 0x01 ghost, 0x02 has-equipment, 0x04 torch, 0x10 peaceful, 0x20 flying.
    pub flags: u8,
    pub name: String,
    /// The subtitle under the name: guild name or role ("Armor Merchant", "Mauler Trainer" …).
    pub guild: String,
}

/// Decode an NPCCreate (0xDA) body.
pub fn decode_npc_create(payload: &[u8]) -> Result<Npc> {
    let mut r = PacketReader::new(payload);
    let object_id = r.u16()?;
    let speed = r.u16()?;
    let heading = r.u16()?;
    let z = r.u16()?;
    let x = r.u32()?;
    let y = r.u32()?;
    r.u16()?; // z-speed factor
    let model = r.u16()?;
    let size = r.u8()?;
    let level = r.u8()? & 0x7F; // high bit marks statues
    let flags = r.u8()?;
    r.u8()?; // max stick distance
    r.u8()?; // flags2 (quest indicators, owner, stealth)
    r.u8()?; // flags3
    r.u16()?; // unknown (1.71+)
    let name = r.pascal_string()?;
    let guild = r.pascal_string()?;
    // one trailing zero byte follows; nothing left to read

    Ok(Npc {
        object_id,
        speed,
        heading,
        x,
        y,
        z,
        model,
        size,
        level,
        flags,
        name,
        guild,
    })
}

/// Another player who came into view (PlayerCreate).
///
/// ## The code is 0x4B, not the enum's 0xD4
/// `eServerPackets` names both `PlayerCreate = 0xD4` and `PlayerCreate172 = 0x4B`. This server
/// sends **0x4B** — that is what appears in the captures, carrying readable character names. Same
/// trap as the mislabeled 0x28/0x29: the enum names codes the live server never emits, so the wire
/// wins.
///
/// ## The body is the 1124 layout, not the 172 one
/// Despite reusing the 172 *code*, `PacketLib1124.SendPlayerCreate` writes a different body than
/// `PacketLib172` does — it leads with three little-endian floats. The 172 layout does not fit the
/// captured bytes at all (it decodes `model` as 16566). Two independent checks pin the 1124 form:
/// the three leading floats decode to plausible Albion coordinates (two guildmates ~450 units
/// apart at an identical z), and the fixed header is exactly 32 bytes — offset 32 lands precisely
/// on the name-length byte in both samples.
///
/// ## Layout (verified against `cap_20260714_230416_conn41042`)
/// ```text
/// [x f32 LE][y f32 LE][z f32 LE]                    ← note: LITTLE-endian, unlike the shorts
/// [session_id u16][object_id u16][heading u16][model u16]
/// [level u8][flags u8]
/// [eye_size][lip_size][mood][eye_colour][hair_colour][face_type][hair_style]  (7 bytes)
/// [0][0][0]
/// [pascal name][pascal guild][pascal last_name][pascal prefix][pascal title]
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct Player {
    pub object_id: u16,
    /// The other player's session id (how the server addresses them), not ours.
    pub session_id: u16,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub heading: u16,
    /// **UNVERIFIED — do not drive rendering from this.** Read big-endian like every other short
    /// here, it decodes to 55562 and 37122 in the two captured samples, which are out of any
    /// sensible model range. Little-endian gives 2777 and 657, and masking to 12 bits gives 2314
    /// and 258 — no reading is corroborated.
    ///
    /// Resolving it against `monsters.csv` is the wrong index anyway: player bodies come from the
    /// `fig3`/`pskin` tables (the A.1 finding — our own avatar arrives as model 0 and is assembled
    /// from race + gender), so a monster-table hit here would be a coincidence, not a confirmation.
    /// Surfaced raw so a later capture with a KNOWN character's race can settle it.
    pub model_unverified: u16,
    pub level: u8,
    /// Realm relative to us, from bits 2–3 (1 Albion, 2 Midgard, 3 Hibernia).
    pub realm: u8,
    /// Raw flag byte: 0x01 dead, 0x02 swimming, 0x10 stealthed, 0x20 wireframe, 0x40 vampiir-fly.
    pub flags: u8,
    pub name: String,
    pub guild: String,
    pub last_name: String,
    /// The other player's own face and hair, straight off the wire.
    ///
    /// These seven bytes are why two Firbolg males on screen are not the same person. Skipping
    /// them and rendering the table's first row instead makes every member of a race identical,
    /// which is the one thing a character-customisation system exists to prevent.
    pub custom: crate::customization::Customization,
    /// Packed Nose (high nibble) and Eyes (low nibble) source slider values.
    pub eye_size: u8,
    /// Packed Lips/Ears (low nibble) and Jaw/Chin (high nibble) source slider values.
    pub lip_size: u8,
}

impl Player {
    /// The complete fig3 appearance advertised by this player-create packet.
    ///
    /// PlayerCreate does not carry `CustomisationStep`. A nonzero morph byte or any discrete
    /// selection means the source client has an authored look; an all-zero legacy record retains
    /// the retail neutral base head.
    #[must_use]
    pub fn appearance(&self) -> crate::customization::AvatarAppearance {
        crate::customization::AvatarAppearance {
            customization: self.custom,
            eye_size: self.eye_size,
            lip_size: self.lip_size,
            facial_morphs_enabled: self.eye_size != 0
                || self.lip_size != 0
                || !self.custom.is_default(),
        }
    }
}

/// Decode a PlayerCreate (0x4B, 1124 body) — see [`Player`] for the layout and its provenance.
pub fn decode_player_create(payload: &[u8]) -> Result<Player> {
    let mut r = PacketReader::new(payload);
    // Position is little-endian float here, matching the player's own position packets (0x20/0xA9)
    // and NOT the big-endian integer coordinates that NPC/object creates use. Both conventions are
    // real in this protocol; see the module header.
    let x = r.f32_le()?;
    let y = r.f32_le()?;
    let z = r.f32_le()?;

    let session_id = r.u16()?;
    let object_id = r.u16()?;
    let heading = r.u16()?;
    let model_unverified = r.u16()?;

    let level = r.u8()?;
    let flags = r.u8()?;
    let realm = (flags >> 2) & 0x03;

    // Face attributes, in wire order. Eye/lip carry the four packed source morph positions;
    // preserving them is required for two figures of one race to have different geometry.
    let eye_size = r.u8()?;
    let lip_size = r.u8()?;
    let mood_type = r.u8()?;
    let eye_color = r.u8()?;
    let hair_color = r.u8()?;
    let face_type = r.u8()?;
    let hair_style = r.u8()?;
    for _ in 0..3 {
        r.u8()?; // 0x00 ×3 (one "new in 1.74", two unknown)
    }

    let name = r.pascal_string()?;
    let guild = r.pascal_string()?;
    // The tail (last name, prefix, title, then optional horse block) is frequently empty and its
    // later fields are version-dependent, so a short read here is tolerated rather than fatal:
    // the name/guild above are what the game layer actually needs.
    let last_name = r.pascal_string().unwrap_or_default();

    Ok(Player {
        object_id,
        session_id,
        x,
        y,
        z,
        heading,
        model_unverified,
        level,
        realm,
        flags,
        name,
        guild,
        last_name,
        custom: crate::customization::Customization {
            eye_color,
            hair_color,
            face_type,
            hair_style,
            mood_type,
        },
        eye_size,
        lip_size,
    })
}

/// Living-model id packed in the low 11 bits of [`Player::model_unverified`].
///
/// OPEN_ORACLE (`GamePlayer.Model` remarks, SoloDAoC): hair colour in the top 3 bits, size in the
/// next 2, **model in the remaining 11** (`model & 0x7FF`). The full ushort is still labelled
/// `model_unverified` because Eden captures have not yet corroborated the packing against a known
/// character on screen — but when the low 11 bits hit a known `eLivingModel`, race+gender are
/// recoverable without guessing a creature mesh.
#[must_use]
pub fn player_living_model_id(model_unverified: u16) -> u16 {
    model_unverified & 0x07FF
}

/// Map a living-model id to `(eRace, fig3 gender)` when it matches OPEN_ORACLE `eLivingModel`.
///
/// Gender uses the fig3 convention (`1` male / `2` female), matching DOL `eGender` (not the DB's
/// 0/1 encoding). Unknown / unmatched ids return `None` — render keeps the honest placeholder.
#[must_use]
pub fn race_gender_from_living_model(living_model: u16) -> Option<(u8, u8)> {
    // Values from SoloDAoC `GlobalConstants.eLivingModel` + `eRace` / `PlayerRace` tables.
    const MALE: u8 = 1;
    const FEMALE: u8 = 2;
    match living_model {
        // Albion
        32 => Some((1, MALE)),   // BritonMale
        35 => Some((1, FEMALE)), // BritonFemale
        39 => Some((3, MALE)),   // HighlanderMale
        43 => Some((3, FEMALE)), // HighlanderFemale
        48 => Some((4, MALE)),   // SaracenMale
        52 => Some((4, FEMALE)), // SaracenFemale
        61 => Some((2, MALE)),   // AvalonianMale
        65 => Some((2, FEMALE)), // AvalonianFemale
        716 => Some((13, MALE)), // InconnuMale
        724 => Some((13, FEMALE)),
        1008 => Some((16, MALE)), // HalfOgreMale
        1020 => Some((16, FEMALE)),
        1395 => Some((19, MALE)), // AlbionMinotaur / Korazh
        // Midgard
        137 => Some((6, MALE)), // TrollMale
        145 => Some((6, FEMALE)),
        503 => Some((5, MALE)), // NorseMale
        507 => Some((5, FEMALE)),
        169 => Some((8, MALE)), // KoboldMale
        177 => Some((8, FEMALE)),
        185 => Some((7, MALE)), // DwarfMale
        193 => Some((7, FEMALE)),
        773 => Some((14, MALE)), // ValkynMale
        781 => Some((14, FEMALE)),
        1051 => Some((17, MALE)), // FrostalfMale
        1063 => Some((17, FEMALE)),
        1407 => Some((20, MALE)), // MidgardMinotaur
        // Hibernia
        286 => Some((10, MALE)), // FirbolgMale
        294 => Some((10, FEMALE)),
        302 => Some((9, MALE)), // CeltMale
        310 => Some((9, FEMALE)),
        318 => Some((12, MALE)), // LurikeenMale
        326 => Some((12, FEMALE)),
        334 => Some((11, MALE)), // ElfMale
        342 => Some((11, FEMALE)),
        700 => Some((15, MALE)), // SylvanMale
        708 => Some((15, FEMALE)),
        1075 => Some((18, MALE)), // SharMale
        1087 => Some((18, FEMALE)),
        1419 => Some((21, MALE)), // HiberniaMinotaur
        _ => None,
    }
}

/// Decode race+gender from the PlayerCreate model short when the low-11 living model is known.
#[must_use]
pub fn race_gender_from_player_model(model_unverified: u16) -> Option<(u8, u8)> {
    race_gender_from_living_model(player_living_model_id(model_unverified))
}

/// A static object announced by ObjectCreate (0xD9): forge, chest, signpost, dropped item…
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticObject {
    pub object_id: u16,
    pub emblem: u16,
    pub heading: u16,
    pub x: u32,
    pub y: u32,
    pub z: u16,
    pub model: u16,
    pub name: String,
}

/// Decode an ObjectCreate (0xD9) body (common prefix; door/carry flags in the tail are
/// tolerated but not surfaced yet — Stream A RTs use Door.json InternalID + DoorState).
pub fn decode_object_create(payload: &[u8]) -> Result<StaticObject> {
    let mut r = PacketReader::new(payload);
    let object_id = r.u16()?;
    let emblem = r.u16()?;
    let heading = r.u16()?;
    let z = r.u16()?;
    let x = r.u32()?;
    let y = r.u32()?;
    let model = r.u16()?;
    r.u16()?; // flag word (realm/attackable bits)
    let name = r.pascal_string()?;

    Ok(StaticObject {
        object_id,
        emblem,
        heading,
        x,
        y,
        z,
        model,
        name,
    })
}

/// A live movement/state update for an already-known entity (ObjectUpdate 0xA1) — the
/// highest-frequency in-world packet (1143 in the golden trace, 115 distinct entities).
/// Mirror of `PacketLib168.SendObjectUpdate`. Coordinates are **zone-local** u16 (big-endian);
/// world coords are `zone_grid_offset * 8192 + local` (z is absolute). Normally UDP, but a
/// TCP-only client (GameOpen flag 0, which we are) receives these over TCP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntityUpdate {
    pub object_id: u16,
    pub speed: u16,
    pub heading: u16,
    /// Zone-local coordinates (add the zone grid offset * 8192 for world X/Y; Z is absolute).
    pub local_x: u16,
    pub local_y: u16,
    pub z: u16,
    /// Object id this entity is targeting (0 = none).
    pub target_id: u16,
    pub health_pct: u8,
    pub flags: u8,
    /// Low byte of the zone's skin id (which zone the local coords belong to).
    pub zone: u8,
}

/// Decode an ObjectUpdate (0xA1) body: ten big-endian u16s then four bytes.
pub fn decode_object_update(payload: &[u8]) -> Result<EntityUpdate> {
    let mut r = PacketReader::new(payload);
    let speed = r.u16()?;
    let heading = r.u16()?;
    let local_x = r.u16()?;
    r.u16()?; // target zone-local X (movement extrapolation hint)
    let local_y = r.u16()?;
    r.u16()?; // target zone-local Y
    let z = r.u16()?;
    r.u16()?; // target Z
    let object_id = r.u16()?;
    let target_id = r.u16()?;
    let health_pct = r.u8()?;
    let flags = r.u8()?;
    let zone = r.u8()?;
    // one trailing byte (target zone) — tolerated if absent

    Ok(EntityUpdate {
        object_id,
        speed,
        heading,
        local_x,
        local_y,
        z,
        target_id,
        health_pct,
        flags,
        zone,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two verbatim PlayerCreate (0x4B) bodies from `cap_20260714_230416_conn41042` — a level 50
    /// and a level 29, both in "Clan Cotswold", standing together.
    const PLAYER_L50: &str = "10f40849e0b8f94800901145000241670\
         71dd90a320444440020010020000000084c696c696c6c796e0d436c616e20436f7473776f6c6400000000";
    const PLAYER_L29: &str = "800f094940b6f94800901145000141690be0\
         91021d0444440000000010000000054665696c650d436c616e20436f7473776f6c6400000000";

    fn unhex(s: &str) -> Vec<u8> {
        let c: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..c.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&c[i..i + 2], 16).unwrap())
            .collect()
    }

    /// The decisive structural check: the fixed header must be exactly 32 bytes, so the pascal
    /// strings land where they land. If any field above them changed width, the name would decode
    /// as garbage rather than failing outright — which is why this asserts on the STRINGS.
    #[test]
    fn player_create_decodes_both_captured_players() {
        let a = decode_player_create(&unhex(PLAYER_L50)).expect("decode l50");
        assert_eq!(a.name, "Lilillyn");
        assert_eq!(a.guild, "Clan Cotswold");
        assert_eq!(a.level, 50);
        assert_eq!(a.session_id, 2);
        assert_eq!(a.object_id, 16743);
        assert_eq!(a.realm, 1, "Clan Cotswold is an Albion guild");

        let b = decode_player_create(&unhex(PLAYER_L29)).expect("decode l29");
        assert_eq!(b.name, "Feile");
        assert_eq!(b.guild, "Clan Cotswold");
        assert_eq!(b.level, 29);
        assert_eq!(b.session_id, 1);
        assert_eq!(b.object_id, 16745);
        assert_eq!(b.realm, 1);
    }

    /// The seven face bytes are the difference between two Firbolgs and one Firbolg twice.
    ///
    /// They were read and thrown away for as long as this decoder existed, which is why every
    /// player of a race rendered with the look tables' first row. These are the real values off
    /// the capture, so the assertion fails the moment the fields shift by a byte — the failure a
    /// hand-built payload cannot produce.
    ///
    /// Note what the capture says about `eye_color`: 32 (0x20) and 0. Read as "low nibble is the
    /// skin tone", both level-capped characters have tone 0, which the tables floor to 1. That is
    /// recorded, not interpreted — the byte is carried faithfully whichever nibble is the tone.
    #[test]
    fn player_create_carries_the_face_and_hair_bytes() {
        let a = decode_player_create(&unhex(PLAYER_L50)).expect("decode l50");
        assert_eq!(
            a.custom,
            crate::customization::Customization {
                eye_color: 32,
                hair_color: 1,
                face_type: 0,
                hair_style: 32,
                mood_type: 0,
            },
            "Lilillyn's authored look"
        );

        let b = decode_player_create(&unhex(PLAYER_L29)).expect("decode l29");
        assert_eq!(b.custom.hair_style, 16, "Feile's hairstyle");
        assert_eq!(b.custom.eye_color, 0);

        // The point of carrying them: these two are not the same person. A decoder that skipped
        // the block returned an equal (default) look for both and the difference vanished.
        assert_ne!(
            a.custom, b.custom,
            "two captured players must not decode to the same look"
        );
    }

    /// Position is three LITTLE-endian floats, unlike the big-endian integer coordinates in
    /// NPC/object creates. Reading them big-endian yields astronomical garbage, so asserting they
    /// land in a sane Albion range pins the byte order.
    #[test]
    fn player_create_positions_are_little_endian_floats() {
        let a = decode_player_create(&unhex(PLAYER_L50)).expect("decode");
        let b = decode_player_create(&unhex(PLAYER_L29)).expect("decode");
        for p in [&a, &b] {
            assert!(
                (500_000.0..600_000.0).contains(&p.x),
                "x {} out of Albion range",
                p.x
            );
            assert!(
                (500_000.0..600_000.0).contains(&p.y),
                "y {} out of Albion range",
                p.y
            );
            assert!((0.0..10_000.0).contains(&p.z), "z {} implausible", p.z);
        }
        // They were standing together — same ground height, a few hundred units apart.
        assert!(
            (a.z - b.z).abs() < 1.0,
            "guildmates should share a ground height"
        );
        assert!((a.x - b.x).abs() < 1000.0);
    }

    /// Heading must be in DAoC's 0..4096 range for both samples. This is the one field adjacent to
    /// the unverified model short, so a silent off-by-one there would show up here.
    #[test]
    fn player_create_heading_is_in_range() {
        for s in [PLAYER_L50, PLAYER_L29] {
            let p = decode_player_create(&unhex(s)).expect("decode");
            assert!(p.heading < 4096, "heading {} out of range", p.heading);
        }
    }

    /// OPEN_ORACLE living-model table: known eLivingModel ids decode; the Cotswold capture's
    /// low-11 values (266 / 258) do **not** — those stay placeholder until a known-character
    /// capture settles the packing.
    #[test]
    fn living_model_maps_oracle_bodies_and_leaves_capture_unknown() {
        assert_eq!(race_gender_from_living_model(43), Some((3, 2))); // HighlanderFemale
        assert_eq!(race_gender_from_living_model(32), Some((1, 1))); // BritonMale
        assert_eq!(race_gender_from_living_model(503), Some((5, 1))); // NorseMale
        assert_eq!(race_gender_from_living_model(302), Some((9, 1))); // CeltMale

        let lil = decode_player_create(&unhex(PLAYER_L50)).unwrap();
        let feile = decode_player_create(&unhex(PLAYER_L29)).unwrap();
        assert_eq!(player_living_model_id(lil.model_unverified), 266);
        assert_eq!(player_living_model_id(feile.model_unverified), 258);
        assert!(race_gender_from_player_model(lil.model_unverified).is_none());
        assert!(race_gender_from_player_model(feile.model_unverified).is_none());
        // Named falsifier: unknown ≠ Highlander Female (race 3, gender 2).
        assert_ne!(
            race_gender_from_living_model(player_living_model_id(lil.model_unverified)),
            Some((3, 2))
        );
        assert_ne!(
            race_gender_from_living_model(player_living_model_id(feile.model_unverified)),
            Some((3, 2))
        );
        // Packed hair bits must not break a known base model.
        assert_eq!(race_gender_from_player_model(0xD000 | 43), Some((3, 2)));
    }

    /// A truncated PlayerCreate must error rather than panic — but a body that stops after the
    /// guild name is NOT truncation, it is the common case (empty last name/prefix/title).
    #[test]
    fn player_create_tolerates_a_missing_tail_but_rejects_a_short_body() {
        let full = unhex(PLAYER_L29);
        // Everything through the guild string, nothing after.
        let through_guild = 32 + 1 + 5 + 1 + 13;
        let p = decode_player_create(&full[..through_guild]).expect("tail is optional");
        assert_eq!(p.name, "Feile");
        assert_eq!(p.last_name, "");

        // Anything shorter than the fixed header cannot decode.
        for len in 0..32 {
            assert!(
                decode_player_create(&full[..len]).is_err(),
                "len {len} should not decode"
            );
        }
    }

    /// A verbatim NPCCreate from the golden trace: Lundeg Tranyth, the Camelot Hills armor
    /// merchant.
    const GOLDEN_NPC: &str = "\
        322e00000422098d000890df0007c6ea0000000934084220000000000e4c75\
        6e646567205472616e7974680e41726d6f72204d65726368616e7400";

    #[test]
    fn decodes_golden_npc_create() {
        let payload: Vec<u8> = hex(GOLDEN_NPC);
        let npc = decode_npc_create(&payload).expect("golden NPCCreate must decode");
        assert_eq!(npc.object_id, 12846);
        assert_eq!(npc.name, "Lundeg Tranyth");
        assert_eq!(npc.guild, "Armor Merchant");
        assert_eq!((npc.x, npc.y, npc.z), (561375, 509674, 2445));
        assert_eq!(npc.level, 8);
        assert_eq!(npc.flags & 0x02, 0x02, "merchant has equipment");
    }

    #[test]
    fn truncated_npc_create_errors_cleanly() {
        let payload: Vec<u8> = hex(GOLDEN_NPC);
        assert!(decode_npc_create(&payload[..20]).is_err());
    }

    #[test]
    fn decodes_golden_object_update() {
        // A verbatim 0xA1 from the golden trace: Lundeg Tranyth (oid 12846) standing still.
        // Its NPCCreate placed it at world (561375, 509674) — the zone-local (12511, 26346)
        // here reconstructs to exactly that via offset 67/59 * 8192, proving the coord model.
        let payload: Vec<u8> = hex("0000042230df000066ea0000098d0000322e000064400000");
        let u = decode_object_update(&payload).expect("golden ObjectUpdate must decode");
        assert_eq!(u.object_id, 12846);
        assert_eq!((u.local_x, u.local_y, u.z), (12511, 26346, 2445));
        assert_eq!(u.health_pct, 100);
        assert_eq!(u.speed, 0);
        // world reconstruction matches the NPCCreate's absolute coords
        assert_eq!(67 * 8192 + u.local_x as u32, 561375);
        assert_eq!(59 * 8192 + u.local_y as u32, 509674);
    }

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
}
