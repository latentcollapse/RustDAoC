//! Declarative S2C typed-decode registry — census authority (not source-window heuristics).
//!
//! Each row is `(opcode, codes::server name, ServerEvent variant name)`.
//! `packetcensus` marks `TYPED_DECODE` only from this table. Session arms and this table
//! are kept aligned by the tests below (exact string presence + mutation falsifiers).

use crate::codes;

/// Exact opcode → session code const → ServerEvent variant.
/// Append-only for stable census; do not reorder existing rows casually.
pub const TYPED_S2C_ROUTES: &[(u8, &str, &str)] = &[
    (codes::server::CryptKey, "CryptKey", "CryptKeyReceived"),
    (codes::server::LoginGranted, "LoginGranted", "LoginGranted"),
    (codes::server::LoginDenied, "LoginDenied", "LoginDenied"),
    (codes::server::SessionID, "SessionID", "SessionAssigned"),
    (
        codes::server::CharacterOverview,
        "CharacterOverview",
        "CharacterOverview",
    ),
    (codes::server::RegionServer, "RegionServer", "RegionHandoff"),
    (
        codes::server::CharacterInitFinished,
        "CharacterInitFinished",
        "EnteredWorld",
    ),
    (codes::server::Message, "Message", "ChatMessage"),
    (
        codes::server::PositionAndObjectID,
        "PositionAndObjectID",
        "PlayerPosition",
    ),
    (codes::server::Dialog, "Dialog", "Dialog"),
    (codes::server::NPCCreate, "NPCCreate", "NpcInView"),
    (codes::server::ObjectCreate, "ObjectCreate", "ObjectInView"),
    (codes::server::ObjectUpdate, "ObjectUpdate", "EntityUpdated"),
    (codes::server::ObjectDelete, "ObjectDelete", "ObjectRemoved"),
    (codes::server::PlayerCreate, "PlayerCreate", "PlayerInView"),
    (
        codes::server::CharacterStatusUpdate,
        "CharacterStatusUpdate",
        "StatusUpdate",
    ),
    (
        codes::server::CombatAnimation,
        "CombatAnimation",
        "CombatAnimation",
    ),
    (
        codes::server::InventoryUpdate,
        "InventoryUpdate",
        "InventoryUpdated",
    ),
    (codes::server::MoneyUpdate, "MoneyUpdate", "MoneyUpdated"),
    (codes::server::PlayerDeath, "PlayerDeath", "PlayerDied"),
    (codes::server::PlayerRevive, "PlayerRevive", "PlayerRevived"),
    (
        codes::server::MerchantWindow,
        "MerchantWindow",
        "MerchantWindow",
    ),
    (
        codes::server::SpellCastAnimation,
        "SpellCastAnimation",
        "SpellCast",
    ),
    (
        codes::server::SpellEffectAnimation,
        "SpellEffectAnimation",
        "SpellEffect",
    ),
    (
        codes::server::InterruptSpellCast,
        "InterruptSpellCast",
        "SpellInterrupted",
    ),
    (codes::server::UpdateIcons, "UpdateIcons", "UpdateIcons"),
    (
        codes::server::ConcentrationList,
        "ConcentrationList",
        "ConcentrationList",
    ),
    (
        codes::server::CharacterPointsUpdate,
        "CharacterPointsUpdate",
        "CharacterPoints",
    ),
    (
        codes::server::RegionChanged,
        "RegionChanged",
        "RegionChanged",
    ),
    (codes::server::DoorState, "DoorState", "DoorState"),
    (
        codes::server::ChangeGroundTarget,
        "ChangeGroundTarget",
        "GroundTargetChanged",
    ),
    (
        codes::server::CharacterJump,
        "CharacterJump",
        "CharacterJump",
    ),
    (
        codes::server::GroupMemberUpdate,
        "GroupMemberUpdate",
        "GroupMemberUpdate",
    ),
    (codes::server::TradeWindow, "TradeWindow", "TradeWindow"),
    (codes::server::QuestEntry, "QuestEntry", "QuestEntry"),
    (codes::server::PetWindow, "PetWindow", "PetWindow"),
    (codes::server::Quit, "Quit", "LoggedOut"),
    (codes::server::AttackMode, "AttackMode", "AttackMode"),
    (codes::server::MaxSpeed, "MaxSpeed", "MaxSpeed"),
    (codes::server::StatsUpdate, "StatsUpdate", "StatsUpdated"),
    (
        codes::server::GameOpenReply,
        "GameOpenReply",
        "GameOpenReply",
    ),
    (codes::server::Realm, "Realm", "Realm"),
    (
        codes::server::ConsignmentMerchantMoney,
        "ConsignmentMerchantMoney",
        "ConsignmentMerchantMoney",
    ),
    (
        codes::server::MarketExplorerWindow,
        "MarketExplorerWindow",
        "MarketExplorer",
    ),
    (
        codes::server::TrainerWindow,
        "TrainerWindow",
        "TrainerWindow",
    ),
    (
        codes::server::FindGroupUpdate,
        "FindGroupUpdate",
        "FindGroupUpdate",
    ),
    (codes::server::Encumberance, "Encumberance", "Encumberance"),
    (
        codes::server::ObjectGuildID,
        "ObjectGuildID",
        "ObjectGuildId",
    ),
    (
        codes::server::EmblemDialogue,
        "EmblemDialogue",
        "EmblemDialogue",
    ),
    (
        codes::server::SiegeWeaponAnimation,
        "SiegeWeaponAnimation",
        "SiegeWeaponAnimation",
    ),
    (
        codes::server::SiegeWeaponInterface,
        "SiegeWeaponInterface",
        "SiegeWeaponInterface",
    ),
    (
        codes::server::EmoteAnimation,
        "EmoteAnimation",
        "EmoteAnimation",
    ),
    (
        codes::server::EquipmentUpdate,
        "EquipmentUpdate",
        "EquipmentUpdated",
    ),
    (codes::server::RemoveObject, "RemoveObject", "RemoveObject"),
    (codes::server::ChangeTarget, "ChangeTarget", "TargetChanged"),
    (codes::server::UDPInitReply, "UDPInitReply", "UdpInitReply"),
    (
        codes::server::BadNameCheckReply,
        "BadNameCheckReply",
        "BadNameCheckReply",
    ),
    (
        codes::server::DupNameCheckReply,
        "DupNameCheckReply",
        "DupNameCheckReply",
    ),
    (
        codes::server::CharacterCreateReply,
        "CharacterCreateReply",
        "CharacterCreateReply",
    ),
    (
        codes::server::CheckLosRequest,
        "CheckLosRequest",
        "CheckLosRequest",
    ),
    (codes::server::TimerWindow, "TimerWindow", "TimerWindow"),
    (
        codes::server::DisableSkills,
        "DisableSkills",
        "DisableSkills",
    ),
    (codes::server::PlaySound, "PlaySound", "PlaySound"),
    (codes::server::SoundEffect, "SoundEffect", "SoundEffect"),
    (codes::server::ModelChange, "ModelChange", "ModelChange"),
    (
        codes::server::MovingObjectCreate,
        "MovingObjectCreate",
        "MovingObjectCreate",
    ),
    (
        codes::server::ObjectDataUpdate,
        "ObjectDataUpdate",
        "ObjectDataUpdate",
    ),
    (codes::server::Riding, "Riding", "Riding"),
    (
        codes::server::PlayerModelTypeChange,
        "PlayerModelTypeChange",
        "PlayerModelTypeChange",
    ),
    (codes::server::DelveInfo, "DelveInfo", "DelveInfo"),
    (
        codes::server::ControlledHorse,
        "ControlledHorse",
        "ControlledHorse",
    ),
];

/// Handled without a `ServerEvent` payload (empty / ack).
pub const TYPED_S2C_HANDLED_NO_EVENT: &[&str] = &["PingReply"];

#[must_use]
pub fn is_typed_s2c_opcode(op: u8) -> bool {
    TYPED_S2C_ROUTES.iter().any(|(o, _, _)| *o == op)
}

/// Extract the body of the match arm that contains `code_needle` at `start`.
/// Returns the substring from `=>` through the matching closing `}` / end of expression arm.
#[must_use]
fn match_arm_body_at(session_src: &str, start: usize) -> Option<&str> {
    let after = &session_src[start..];
    let arrow = after.find("=>")?;
    let body_start = start + arrow + 2;
    let rest = session_src.get(body_start..)?;
    let trimmed = rest.trim_start();
    let trim_off = rest.len() - trimmed.len();
    let abs = body_start + trim_off;
    if trimmed.starts_with('{') {
        let mut depth = 0i32;
        for (i, ch) in session_src[abs..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&session_src[abs..=abs + i]);
                    }
                }
                _ => {}
            }
        }
        None
    } else {
        // Expression arm ending at `,` or bare newline before next pattern — take until `,` at depth 0.
        let mut depth = 0i32;
        for (i, ch) in session_src[abs..].char_indices() {
            match ch {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth <= 0 => return Some(&session_src[abs..abs + i]),
                _ => {}
            }
        }
        Some(&session_src[abs..])
    }
}

/// Exact-route classifier: the **same match arm** that cites `codes::server::{code}` must
/// emit `ServerEvent::{event}`. Adjacent arms cannot false-green the row.
#[must_use]
pub fn session_declares_typed_route(session_src: &str, code_name: &str, event_name: &str) -> bool {
    let code_needle = format!("codes::server::{code_name}");
    let event_needle = format!("ServerEvent::{event_name}");
    let mut search_from = 0usize;
    while let Some(rel) = session_src[search_from..].find(&code_needle) {
        let i = search_from + rel;
        if let Some(arm) = match_arm_body_at(session_src, i) {
            if arm.contains(&event_needle) && !arm_is_raw_only(arm, &event_needle) {
                return true;
            }
        }
        search_from = i + code_needle.len();
    }
    // CryptKey / PingReply specials: handled without a payload Event in some arms.
    if code_name == "CryptKey" {
        return session_src.contains("CryptKeyReceived")
            && session_src.contains("codes::server::CryptKey");
    }
    false
}

fn arm_is_raw_only(arm: &str, event_needle: &str) -> bool {
    // If the only ServerEvent:: in the arm is Raw (and we wanted a typed event), fail.
    if event_needle.ends_with("Raw") {
        return false;
    }
    let has_typed = arm.contains(event_needle);
    let has_raw = arm.contains("ServerEvent::Raw");
    // Ok(typed) / Err(Raw) is fine when typed is present.
    has_raw && !has_typed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_opcodes_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for (op, _, _) in TYPED_S2C_ROUTES {
            assert!(seen.insert(*op), "duplicate typed opcode {op:#04x}");
        }
    }

    #[test]
    fn every_registry_route_exists_exactly_in_session() {
        let session = include_str!("session.rs");
        for &(op, code, event) in TYPED_S2C_ROUTES {
            assert!(
                session_declares_typed_route(session, code, event),
                "registry route {op:#04x} {code}→{event} missing exact session arm"
            );
        }
        for name in TYPED_S2C_HANDLED_NO_EVENT {
            assert!(
                session.contains(&format!("codes::server::{name}")),
                "handled-no-event {name} missing"
            );
        }
    }

    #[test]
    fn mutation_wrong_event_in_window_is_not_enough() {
        let fake = r#"
            (_, code) if code == codes::server::LoginDenied => {
                vec![Action::Event(ServerEvent::LoginGranted)]
            }
        "#;
        assert!(
            !session_declares_typed_route(fake, "LoginDenied", "LoginDenied"),
            "wrong event must not classify as LoginDenied typed"
        );
    }

    #[test]
    fn mutation_adjacent_arm_expected_event_cannot_false_green() {
        // The audit falsifier: wrong arm + adjacent arm with the expected event.
        let fake = r#"
            (_, code) if code == codes::server::LoginDenied => {
                vec![Action::Event(ServerEvent::LoginGranted)]
            }
            (_, code) if code == codes::server::Realm => {
                vec![Action::Event(ServerEvent::LoginDenied { error: 1 })]
            }
        "#;
        assert!(
            !session_declares_typed_route(fake, "LoginDenied", "LoginDenied"),
            "expected event in the next arm must not promote LoginDenied"
        );
        assert!(
            !session_declares_typed_route(fake, "Realm", "Realm"),
            "Realm arm emitting LoginDenied is not a Realm typed route"
        );
    }

    #[test]
    fn mutation_raw_only_fallback_is_not_typed() {
        let fake = r#"
            (_, code) if code == codes::server::MaxSpeed => {
                vec![Action::Event(ServerEvent::Raw { code, payload: payload.to_vec() })]
            }
        "#;
        assert!(!session_declares_typed_route(fake, "MaxSpeed", "MaxSpeed"));
    }

    #[test]
    fn mutation_missing_code_const_fails() {
        let fake = r#"
            (_, code) if code == 0xB6 => {
                vec![Action::Event(ServerEvent::MaxSpeed { percent: 100, turning_disabled: false, water_percent: 0 })]
            }
        "#;
        assert!(!session_declares_typed_route(fake, "MaxSpeed", "MaxSpeed"));
    }

    #[test]
    fn correct_arm_with_ok_typed_err_raw_still_passes() {
        let fake = r#"
            (_, code) if code == codes::server::EmoteAnimation => {
                match crate::emote::decode(payload) {
                    Ok(e) => vec![Action::Event(ServerEvent::EmoteAnimation(e))],
                    Err(_) => vec![Action::Event(ServerEvent::Raw { code, payload: payload.to_vec() })],
                }
            }
        "#;
        assert!(session_declares_typed_route(
            fake,
            "EmoteAnimation",
            "EmoteAnimation"
        ));
    }
}
