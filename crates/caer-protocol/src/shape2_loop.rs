//! Shape-2 first continuous-loop codecs still marked IN_SCOPE_ABSENT before this repair.
//!
//! OPEN_ORACLE PacketLib168 / PacketLib171 / PacketLib180 / PacketLib1110 + Client/168 handlers.
//! C2S encoders and S2C decoders live together so session product paths stay one hop from wire.

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

// ---------------------------------------------------------------------------
// S2C — name / create
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadNameCheckReply {
    pub name: String,
    pub account: String,
    /// Oracle: `bad ? 0x0 : 0x1` — 0 = rejected, 1 = accepted.
    pub accepted: bool,
}

pub fn decode_bad_name_check_reply(payload: &[u8]) -> Result<BadNameCheckReply> {
    let mut r = PacketReader::new(payload);
    let name = r.fixed_string(30)?;
    let account = r.fixed_string(20)?;
    let flag = r.u8()?;
    let _pad = r.bytes(3)?;
    Ok(BadNameCheckReply {
        name,
        account,
        accepted: flag != 0,
    })
}

#[must_use]
pub fn encode_bad_name_check_reply(name: &str, account: &str, accepted: bool) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.fixed_string(name, 30)
        .fixed_string(account, 20)
        .u8(if accepted { 1 } else { 0 })
        .bytes(&[0, 0, 0]);
    w.into_bytes()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DupNameCheckReply {
    pub name: String,
    pub account: String,
    /// 0 free, 1 invalid, 2 already exists (DupNameCheckRequestHandler).
    pub result: u8,
}

pub fn decode_dup_name_check_reply(payload: &[u8]) -> Result<DupNameCheckReply> {
    let mut r = PacketReader::new(payload);
    let name = r.fixed_string(30)?;
    let account = r.fixed_string(20)?;
    let result = r.u8()?;
    let _pad = r.bytes(3)?;
    Ok(DupNameCheckReply {
        name,
        account,
        result,
    })
}

#[must_use]
pub fn encode_dup_name_check_reply(name: &str, account: &str, result: u8) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.fixed_string(name, 30)
        .fixed_string(account, 20)
        .u8(result)
        .bytes(&[0, 0, 0]);
    w.into_bytes()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterCreateReply {
    pub name: String,
}

pub fn decode_character_create_reply(payload: &[u8]) -> Result<CharacterCreateReply> {
    let mut r = PacketReader::new(payload);
    Ok(CharacterCreateReply {
        name: r.fixed_string(24)?,
    })
}

#[must_use]
pub fn encode_character_create_reply(name: &str) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.fixed_string(name, 24);
    w.into_bytes()
}

// ---------------------------------------------------------------------------
// S2C — combat / UI / presentation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckLosRequest {
    pub checker_oid: u16,
    pub target_oid: u16,
}

pub fn decode_check_los_request(payload: &[u8]) -> Result<CheckLosRequest> {
    let mut r = PacketReader::new(payload);
    let checker_oid = r.u16()?;
    let target_oid = r.u16()?;
    let _a = r.u16()?;
    let _b = r.u16()?;
    Ok(CheckLosRequest {
        checker_oid,
        target_oid,
    })
}

#[must_use]
pub fn encode_check_los_request(checker_oid: u16, target_oid: u16) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u16(checker_oid).u16(target_oid).u16(0).u16(0);
    w.into_bytes()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerWindow {
    pub seconds: u16,
    pub title: String,
    pub open: bool,
}

pub fn decode_timer_window(payload: &[u8]) -> Result<TimerWindow> {
    let mut r = PacketReader::new(payload);
    let seconds = r.u16()?;
    let title_len = r.u8()? as usize;
    let flag = r.u8()?;
    let title = if title_len == 0 {
        String::new()
    } else {
        let raw = r.bytes(title_len)?;
        raw.iter().map(|&b| b as char).collect()
    };
    Ok(TimerWindow {
        seconds,
        title,
        open: !(seconds == 0 && flag == 0 && title_len == 0),
    })
}

#[must_use]
pub fn encode_timer_window(seconds: u16, title: &str) -> Vec<u8> {
    let mut w = PacketWriter::new();
    let bytes: Vec<u8> = title.chars().map(|c| c as u8).collect();
    let len = bytes.len().min(255);
    w.u16(seconds).u8(len as u8).u8(1).bytes(&bytes[..len]);
    w.into_bytes()
}

#[must_use]
pub fn encode_timer_window_close() -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u16(0).u8(0).u8(0);
    w.into_bytes()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisableSkills {
    pub duration: u16,
    pub code: u8,
    /// Hybrid (code 1): (index, duration). List spells (code 2): packed as (line<<8|spell, 0).
    pub entries: Vec<(u16, u16)>,
}

pub fn decode_disable_skills(payload: &[u8]) -> Result<DisableSkills> {
    let mut r = PacketReader::new(payload);
    let duration = r.u16()?;
    let count = r.u8()? as usize;
    let code = r.u8()?;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        if code == 2 {
            let line = r.u8()? as u16;
            let spell = r.u8()? as u16;
            entries.push(((line << 8) | spell, duration));
        } else {
            let index = r.u16()?;
            let dur = r.u16()?;
            entries.push((index, dur));
        }
    }
    Ok(DisableSkills {
        duration,
        code,
        entries,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaySound {
    pub sound_type: u16,
    pub sound_id: u16,
}

pub fn decode_play_sound(payload: &[u8]) -> Result<PlaySound> {
    let mut r = PacketReader::new(payload);
    let sound_type = r.u16()?;
    let sound_id = r.u16()?;
    let _pad = r.bytes(8.min(r.remaining()))?;
    Ok(PlaySound {
        sound_type,
        sound_id,
    })
}

#[must_use]
pub fn encode_play_sound(sound_type: u16, sound_id: u16) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u16(sound_type).u16(sound_id).bytes(&[0u8; 8]);
    w.into_bytes()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoundEffect {
    pub sound_id: u16,
    pub zone_id: u16,
    pub x: u16,
    pub y: u16,
    pub z: u16,
    pub radius: u16,
}

pub fn decode_sound_effect(payload: &[u8]) -> Result<SoundEffect> {
    let mut r = PacketReader::new(payload);
    Ok(SoundEffect {
        sound_id: r.u16()?,
        zone_id: r.u16()?,
        x: r.u16()?,
        y: r.u16()?,
        z: r.u16()?,
        radius: r.u16()?,
    })
}

#[must_use]
pub fn encode_sound_effect(s: &SoundEffect) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u16(s.sound_id)
        .u16(s.zone_id)
        .u16(s.x)
        .u16(s.y)
        .u16(s.z)
        .u16(s.radius);
    w.into_bytes()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelChange {
    pub object_id: u16,
    pub new_model: u16,
    pub new_size: u8,
}

pub fn decode_model_change(payload: &[u8]) -> Result<ModelChange> {
    let mut r = PacketReader::new(payload);
    let object_id = r.u16()?;
    let new_model = r.u16()?;
    // WriteIntLowEndian(newSize) — size in low byte of LE u32.
    let size_le = r.u32_le()?;
    Ok(ModelChange {
        object_id,
        new_model,
        new_size: (size_le & 0xFF) as u8,
    })
}

#[must_use]
pub fn encode_model_change(m: &ModelChange) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u16(m.object_id)
        .u16(m.new_model)
        .u32_le(u32::from(m.new_size));
    w.into_bytes()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovingObjectCreate {
    pub object_id: u16,
    pub heading: u16,
    pub z: u16,
    pub x: u32,
    pub y: u32,
    pub model: u16,
    pub flags: u16,
    pub emblem: u16,
    pub name: String,
}

pub fn decode_moving_object_create(payload: &[u8]) -> Result<MovingObjectCreate> {
    let mut r = PacketReader::new(payload);
    let object_id = r.u16()?;
    let _unk = r.u16()?;
    let heading = r.u16()?;
    let z = r.u16()?;
    let x = r.u32()?;
    let y = r.u32()?;
    let model = r.u16()?;
    let flags = r.u16()?;
    let emblem = r.u16()?;
    let _pad = r.u16()?;
    let _pad2 = r.u32()?;
    let name = r.pascal_string()?;
    let _trail = if r.remaining() > 0 { r.u8()? } else { 0 };
    Ok(MovingObjectCreate {
        object_id,
        heading,
        z,
        x,
        y,
        model,
        flags,
        emblem,
        name,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectDataUpdate {
    pub object_id: u16,
    pub level: u8,
    pub guild: String,
    pub name: String,
    pub strings_omitted: bool,
}

pub fn decode_object_data_update(payload: &[u8]) -> Result<ObjectDataUpdate> {
    let mut r = PacketReader::new(payload);
    let object_id = r.u16()?;
    let _pad = r.u8()?;
    let level = r.u8()?;
    if r.remaining() == 0 {
        return Ok(ObjectDataUpdate {
            object_id,
            level,
            guild: String::new(),
            name: String::new(),
            strings_omitted: true,
        });
    }
    let marker = r.peek(1)?[0];
    if marker == 0xFF {
        let _ = r.u8()?;
        return Ok(ObjectDataUpdate {
            object_id,
            level,
            guild: String::new(),
            name: String::new(),
            strings_omitted: true,
        });
    }
    let guild = r.pascal_string()?;
    let name = if r.remaining() > 0 {
        r.pascal_string()?
    } else {
        String::new()
    };
    Ok(ObjectDataUpdate {
        object_id,
        level,
        guild,
        name,
        strings_omitted: false,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Riding {
    pub rider_oid: u16,
    pub steed_oid: u16,
    pub mounted: bool,
    pub slot: u8,
}

pub fn decode_riding(payload: &[u8]) -> Result<Riding> {
    let mut r = PacketReader::new(payload);
    let rider_oid = r.u16()?;
    let steed_oid = r.u16()?;
    let mounted = r.u8()? != 0;
    let slot = r.u8()?;
    let _pad = r.u16()?;
    Ok(Riding {
        rider_oid,
        steed_oid,
        mounted,
        slot,
    })
}

#[must_use]
pub fn encode_riding(r: &Riding) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u16(r.rider_oid)
        .u16(r.steed_oid)
        .u8(if r.mounted { 1 } else { 0 })
        .u8(r.slot)
        .u16(0);
    w.into_bytes()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerModelTypeChange {
    pub object_id: u16,
    pub model_type: u8,
}

pub fn decode_player_model_type_change(payload: &[u8]) -> Result<PlayerModelTypeChange> {
    let mut r = PacketReader::new(payload);
    let object_id = r.u16()?;
    let model_type = r.u8()?;
    let _unused = r.u8()?;
    Ok(PlayerModelTypeChange {
        object_id,
        model_type,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelveInfo {
    pub info: String,
}

pub fn decode_delve_info(payload: &[u8]) -> Result<DelveInfo> {
    let end = payload
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(payload.len());
    Ok(DelveInfo {
        info: payload[..end].iter().map(|&b| b as char).collect(),
    })
}

#[must_use]
pub fn encode_delve_info(info: &str) -> Vec<u8> {
    let mut w = PacketWriter::new();
    let bytes: Vec<u8> = info.chars().map(|c| c as u8).take(2048).collect();
    w.bytes(&bytes).u8(0);
    w.into_bytes()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlledHorse {
    pub object_id: u16,
    pub horse_id: u8,
    pub barding: u8,
    pub barding_color: u16,
    pub saddle: u8,
    pub saddle_color: u8,
    pub active: bool,
}

pub fn decode_controlled_horse(payload: &[u8]) -> Result<ControlledHorse> {
    let mut r = PacketReader::new(payload);
    let object_id = r.u16()?;
    if r.remaining() < 6 {
        return Ok(ControlledHorse {
            object_id,
            horse_id: 0,
            barding: 0,
            barding_color: 0,
            saddle: 0,
            saddle_color: 0,
            active: false,
        });
    }
    let horse_id = r.u8()?;
    let barding = r.u8()?;
    let barding_color = r.u16()?;
    let saddle = r.u8()?;
    let saddle_color = r.u8()?;
    let active = horse_id != 0 || barding != 0 || saddle != 0;
    Ok(ControlledHorse {
        object_id,
        horse_id,
        barding,
        barding_color,
        saddle,
        saddle_color,
        active,
    })
}

// ---------------------------------------------------------------------------
// C2S encoders
// ---------------------------------------------------------------------------

/// BadNameCheck 0xC2 — FillString(name, 30). Attribute `0x6A ^ 168`.
#[must_use]
pub fn encode_bad_name_check(name: &str) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.fixed_string(name, 30);
    w.into_bytes()
}

/// DupNameCheck 0xCB — 1126+ FillString(name, 24).
#[must_use]
pub fn encode_dup_name_check_1126(name: &str) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.fixed_string(name, 24);
    w.into_bytes()
}

/// UDPInitRequest 0x14 — 1124+: FillString(ip, 20) + port u16.
#[must_use]
pub fn encode_udp_init_request_1124(local_ip: &str, local_port: u16) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.fixed_string(local_ip, 20).u16(local_port);
    w.into_bytes()
}

/// UDPPing 0xF2 — 1124+ keepalive (empty body accepted).
#[must_use]
pub fn encode_udp_ping() -> Vec<u8> {
    Vec::new()
}

/// CheckLOSResponse 0xD0 — checker, target, response, pad.
#[must_use]
pub fn encode_check_los_response(checker_oid: u16, target_oid: u16, response: u16) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u16(checker_oid).u16(target_oid).u16(response).u16(0);
    w.into_bytes()
}

#[must_use]
pub fn encode_disband_from_group() -> Vec<u8> {
    Vec::new()
}

#[must_use]
pub fn encode_dismount() -> Vec<u8> {
    Vec::new()
}

#[must_use]
pub fn encode_object_update_request() -> Vec<u8> {
    Vec::new()
}

/// CreatePlayerRequest 0xD5 — 1126+ session id u32 LE.
#[must_use]
pub fn encode_create_player_request_1126(client_id: u32) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u32_le(client_id);
    w.into_bytes()
}

#[must_use]
pub fn encode_remove_quest_request(quest_index: u16) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u16(0).u16(quest_index).u16(0).u16(0);
    w.into_bytes()
}

#[must_use]
pub fn encode_quest_reward_chosen(
    count_chosen: u8,
    items_chosen: &[u8; 8],
    quest_id: u16,
    quest_giver_id: u16,
) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u8(1).u8(count_chosen);
    for b in items_chosen {
        w.u8(*b);
    }
    w.u16(0).u16(0).u16(0).u16(quest_id).u16(quest_giver_id);
    w.into_bytes()
}

#[must_use]
pub fn encode_remove_concentration_effect(index: u8) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u8(index);
    w.into_bytes()
}

#[must_use]
pub fn encode_cancels_effect(effect_id: u16) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u16(effect_id);
    w.into_bytes()
}

#[must_use]
pub fn encode_detail_request(object_type: u16, extra_id: u32, object_id: u16) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u16(object_type).u32(extra_id).u16(object_id);
    w.into_bytes()
}

#[must_use]
pub fn encode_appraise_item(player_x: u32, player_y: u32, id: u16, item_slot: u16) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u32(player_x).u32(player_y).u16(id).u16(item_slot);
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_name_check_round_trip() {
        let body = encode_bad_name_check_reply("Testchar", "acct", true);
        let d = decode_bad_name_check_reply(&body).unwrap();
        assert_eq!(d.name, "Testchar");
        assert!(d.accepted);
    }

    #[test]
    fn timer_open_and_close() {
        let open = encode_timer_window(30, "Siege");
        let d = decode_timer_window(&open).unwrap();
        assert!(d.open);
        assert_eq!(d.seconds, 30);
        assert_eq!(d.title, "Siege");
        let close = encode_timer_window_close();
        let c = decode_timer_window(&close).unwrap();
        assert!(!c.open);
    }

    #[test]
    fn los_request_response_pair() {
        let req = encode_check_los_request(10, 20);
        let d = decode_check_los_request(&req).unwrap();
        assert_eq!(d.checker_oid, 10);
        let clear = encode_check_los_response(10, 20, 0x100);
        let blocked = encode_check_los_response(10, 20, 0);
        assert_eq!(clear.len(), 8);
        assert_eq!(blocked.len(), 8);
        assert_ne!(clear, blocked, "clear vs blocked must differ on the wire");
    }

    #[test]
    fn los_missing_target_and_stale_layout_still_decode_request() {
        let req = encode_check_los_request(1, 0);
        let d = decode_check_los_request(&req).unwrap();
        assert_eq!(d.target_oid, 0);
        // Truncated payload must fail closed (no invented clear).
        assert!(decode_check_los_request(&[0, 1]).is_err());
    }

    #[test]
    fn riding_and_model_change() {
        let body = encode_riding(&Riding {
            rider_oid: 1,
            steed_oid: 2,
            mounted: true,
            slot: 0,
        });
        assert!(decode_riding(&body).unwrap().mounted);
        let m = encode_model_change(&ModelChange {
            object_id: 5,
            new_model: 0x1234,
            new_size: 50,
        });
        let d = decode_model_change(&m).unwrap();
        assert_eq!(d.new_size, 50);
    }

    #[test]
    fn udp_init_request_1124_layout() {
        let body = encode_udp_init_request_1124("127.0.0.1", 10412);
        assert_eq!(body.len(), 22);
    }
}
