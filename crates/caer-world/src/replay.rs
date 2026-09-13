//! Replay a captured session's server stream into `ServerEvent`s for the world model.
//!
//! This bypasses the session state machine on purpose: the machine is phase-gated for *driving*
//! a live connection, but for feeding the world model from a passive capture we just want every
//! entity packet decoded, in order. We scan the server→client frames and turn the world-
//! relevant codes (NPCCreate, ObjectCreate, ObjectUpdate, our own PositionAndObjectID) into
//! events. Everything else is skipped — this is a world-model feeder, not a protocol test (the
//! golden-replay test in `caer-capture` already guards decode correctness end to end).

use caer_protocol::codes;
use caer_protocol::entities::{
    decode_npc_create, decode_object_create, decode_object_update, decode_player_create,
};
use caer_protocol::framing::ServerPacketHeader;
use caer_protocol::session::ServerEvent;

/// One direction byte in the `.bin` record format `[dir:1][millis:8 BE][len:4 BE][raw]`.
const DIR_S2C: u8 = 0x02;

/// Decode a captured `.bin` trace into the ordered stream of world-relevant events.
#[must_use]
pub fn decode_trace(mut buf: &[u8]) -> Vec<ServerEvent> {
    let mut events = Vec::new();
    while buf.len() >= 13 {
        let dir = buf[0];
        let len = u32::from_be_bytes([buf[9], buf[10], buf[11], buf[12]]) as usize;
        let start = 13;
        let end = start + len;
        if end > buf.len() {
            break;
        }
        if dir == DIR_S2C {
            decode_stream(&buf[start..end], &mut events);
        }
        buf = &buf[end..];
    }
    events
}

/// Split one server→client byte stream into frames and emit the world-relevant events.
fn decode_stream(mut stream: &[u8], out: &mut Vec<ServerEvent>) {
    while let Ok(Some((header, payload, used))) = ServerPacketHeader::decode_prefix(stream) {
        match header.code {
            c if c == codes::server::NPCCreate => {
                if let Ok(npc) = decode_npc_create(payload) {
                    out.push(ServerEvent::NpcInView(npc));
                }
            }
            c if c == codes::server::ObjectCreate => {
                if let Ok(obj) = decode_object_create(payload) {
                    out.push(ServerEvent::ObjectInView(obj));
                }
            }
            c if c == codes::server::PlayerCreate => {
                if let Ok(p) = decode_player_create(payload) {
                    out.push(ServerEvent::PlayerInView(p));
                }
            }
            // Entity removal matters to a replay as much as creation: without it a replayed world
            // ends up holding every entity the session ever saw, which is exactly the bug this
            // packet fixes in the live client.
            c if c == codes::server::ObjectDelete && payload.len() >= 2 => {
                out.push(ServerEvent::ObjectRemoved {
                    object_id: u16::from_be_bytes([payload[0], payload[1]]),
                });
            }
            c if c == codes::server::ObjectUpdate => {
                if let Ok(u) = decode_object_update(payload) {
                    out.push(ServerEvent::EntityUpdated(u));
                }
            }
            c if c == codes::server::EquipmentUpdate => {
                if let Ok(e) = caer_protocol::equipment::decode(payload) {
                    out.push(ServerEvent::EquipmentUpdated(e));
                }
            }
            c if c == codes::server::CombatAnimation => {
                if let Ok(a) = caer_protocol::combat_anim::decode(payload) {
                    out.push(ServerEvent::CombatAnimation(a));
                }
            }
            c if c == codes::server::InventoryUpdate => {
                if let Ok(u) = caer_protocol::inventory::decode(payload) {
                    out.push(ServerEvent::InventoryUpdated(u));
                }
            }
            c if c == codes::server::MoneyUpdate => {
                if let Ok(m) = caer_protocol::money::decode(payload) {
                    out.push(ServerEvent::MoneyUpdated(m));
                }
            }
            c if c == codes::server::PlayerDeath => {
                if let Ok(d) = caer_protocol::death::decode_death(payload) {
                    out.push(ServerEvent::PlayerDied(d));
                }
            }
            c if c == codes::server::PlayerRevive => {
                if let Ok(v) = caer_protocol::death::decode_revive(payload) {
                    out.push(ServerEvent::PlayerRevived(v));
                }
            }
            c if c == codes::server::MerchantWindow => {
                if let Ok(w) = caer_protocol::merchant::decode(payload) {
                    out.push(ServerEvent::MerchantWindow(w));
                }
            }
            c if c == codes::server::SpellCastAnimation => {
                if let Ok(c) = caer_protocol::spells::decode_cast(payload) {
                    out.push(ServerEvent::SpellCast(c));
                }
            }
            c if c == codes::server::SpellEffectAnimation => {
                if let Ok(e) = caer_protocol::spells::decode_effect(payload) {
                    out.push(ServerEvent::SpellEffect(e));
                }
            }
            c if c == codes::server::InterruptSpellCast => {
                if let Ok(i) = caer_protocol::spells::decode_interrupt(payload) {
                    out.push(ServerEvent::SpellInterrupted(i));
                }
            }
            c if c == codes::server::UpdateIcons => {
                if let Ok(u) = caer_protocol::effects::decode_update_icons(payload) {
                    out.push(ServerEvent::UpdateIcons(u));
                }
            }
            c if c == codes::server::ConcentrationList => {
                if let Ok(l) = caer_protocol::effects::decode_concentration_list(payload) {
                    out.push(ServerEvent::ConcentrationList(l));
                }
            }
            c if c == codes::server::CharacterPointsUpdate => {
                if let Ok(p) = caer_protocol::points::decode_points(payload) {
                    out.push(ServerEvent::CharacterPoints(p));
                }
            }
            c if c == codes::server::PetWindow => {
                if let Ok(p) = caer_protocol::pets::decode_pet_window(payload) {
                    out.push(ServerEvent::PetWindow(p));
                }
            }
            _ => {}
        }
        stream = &stream[used..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorldState;

    /// The frozen golden trace, reused from `caer-capture`'s fixture.
    const GOLDEN: &[u8] = include_bytes!("../tests/fixtures/rustdaoc_login_20260714.bin");

    #[test]
    fn golden_trace_populates_the_world() {
        let events = decode_trace(GOLDEN);
        assert!(!events.is_empty(), "trace should yield entity events");

        let mut w = WorldState::new();
        for ev in &events {
            w.apply(ev);
        }

        // The real Camelot Hills spawn: ~115 distinct entities move in the trace.
        assert!(
            w.len() >= 100,
            "expected the visible-area population, got {}",
            w.len()
        );

        // Ground truth: Lundeg Tranyth (the armor merchant, oid 12846) is present and his
        // ObjectUpdate-driven position reconstructs to his real world coordinates.
        let lundeg = w.get(12846).expect("Lundeg Tranyth should be in the world");
        assert_eq!(lundeg.name, "Lundeg Tranyth");
        assert_eq!(lundeg.pos[0], 561375);
        assert_eq!(lundeg.pos[1], 509674);

        // Ground-truth invariant: every entity known only by an early update eventually gets
        // its create — nothing is left unresolved once the whole session has been replayed.
        // (updates_before_create > 0 is fine and expected: TCP batching interleaves them.)
        assert_eq!(
            w.unresolved(),
            0,
            "every placeholder resolved to a real create"
        );
    }
}
