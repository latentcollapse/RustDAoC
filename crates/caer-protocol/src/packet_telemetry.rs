//! Fail-closed packet-gap telemetry. Headless, deterministic, no network.
//!
//! Unknown opcodes, malformed payloads that become [`crate::session::ServerEvent::Raw`],
//! and typed events that reach the live drain without a product consumer stay Raw /
//! applied as before — they are also recorded here so they cannot disappear from evidence.

use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PacketGap {
    UnknownOpcode { code: u8 },
    DecodeBecameRaw { code: u8 },
    UnconsumedTyped { event: &'static str },
}

impl PacketGap {
    #[must_use]
    pub fn as_tsv_row(&self) -> String {
        match self {
            Self::UnknownOpcode { code } => format!("unknown_opcode\t0x{code:02X}\t-"),
            Self::DecodeBecameRaw { code } => format!("decode_became_raw\t0x{code:02X}\t-"),
            Self::UnconsumedTyped { event } => format!("unconsumed_typed\t-\t{event}"),
        }
    }
}

static GAPS: Mutex<Vec<PacketGap>> = Mutex::new(Vec::new());

pub fn reset() {
    GAPS.lock().expect("packet telemetry mutex").clear();
}

pub fn record(gap: PacketGap) {
    GAPS.lock().expect("packet telemetry mutex").push(gap);
}

pub fn record_unknown(code: u8) {
    record(PacketGap::UnknownOpcode { code });
}

pub fn record_malformed(code: u8) {
    record(PacketGap::DecodeBecameRaw { code });
}

pub fn record_unconsumed(event: &'static str) {
    record(PacketGap::UnconsumedTyped { event });
}

#[must_use]
pub fn snapshot() -> Vec<PacketGap> {
    GAPS.lock().expect("packet telemetry mutex").clone()
}

#[must_use]
pub fn render_tsv() -> String {
    let mut s = String::from("kind\topcode_hex\tevent\n");
    for g in snapshot() {
        s.push_str(&g.as_tsv_row());
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codes;
    use crate::framing::ServerPacketHeader;
    use crate::session::SessionState;

    fn srv(code: u8) -> ServerPacketHeader {
        ServerPacketHeader { size: 0, code }
    }

    #[test]
    fn unknown_opcode_is_reported_and_still_raw() {
        reset();
        let mut s = SessionState::new("a", "b");
        let actions = s.on_server_packet(srv(0x00), &[]);
        assert!(
            actions.iter().any(|a| matches!(
                a,
                crate::session::Action::Event(crate::session::ServerEvent::Raw { code: 0x00, .. })
            )),
            "unknown opcode must remain Raw"
        );
        assert!(
            snapshot()
                .iter()
                .any(|g| matches!(g, PacketGap::UnknownOpcode { code: 0x00 })),
            "report:\n{}",
            render_tsv()
        );
    }

    #[test]
    fn malformed_typed_payload_is_reported_as_raw() {
        reset();
        let mut s = SessionState::new("a", "b");
        s.begin();
        s.on_server_packet(srv(codes::server::CryptKey), &[]);
        s.on_server_packet(srv(codes::server::LoginGranted), &[]);
        s.on_server_packet(srv(codes::server::SessionID), &[0x07, 0x00]);
        s.on_server_packet(srv(codes::server::Realm), &[0x00]);
        let _ = s.request_character_overview(1);
        let mut empty = vec![0u8; 16];
        empty[0] = 1;
        s.on_server_packet(srv(codes::server::CharacterOverview), &empty);
        s.select_character(0);
        s.on_server_packet(srv(codes::server::CharacterInitFinished), &[0x00]);
        reset();
        let actions = s.on_server_packet(srv(codes::server::InventoryUpdate), &[0x00]);
        assert!(actions.iter().any(|a| matches!(
            a,
            crate::session::Action::Event(crate::session::ServerEvent::Raw {
                code: codes::server::InventoryUpdate,
                ..
            })
        )));
        assert!(
            snapshot().iter().any(|g| matches!(
                g,
                PacketGap::DecodeBecameRaw {
                    code: codes::server::InventoryUpdate
                }
            )),
            "report:\n{}",
            render_tsv()
        );
    }
}
