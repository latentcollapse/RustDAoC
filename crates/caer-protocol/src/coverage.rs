//! Explicit packet coverage stages. A typed decode or `codes::server::*` import is **not**
//! product support. Only names listed here count for STATE_APPLY / PRODUCT_CONSUMER /
//! SCENARIO_PROOF. SCENARIO_PROOF is never `PLAYER_SCENARIO`.

use crate::typed_s2c_registry::TYPED_S2C_ROUTES;

/// WorldState / EcoState match arms that mutate simulation state.
pub const STATE_APPLY_EVENTS: &[&str] = &[
    "NpcInView",
    "ObjectInView",
    "PlayerInView",
    "EntityUpdated",
    "StatusUpdate",
    "CharacterSheet",
    "StatsUpdated",
    "AttackMode",
    "EmoteAnimation",
    "SiegeWeaponAnimation",
    "SiegeWeaponInterface",
    "EquipmentUpdated",
    "InventoryUpdated",
    "MoneyUpdated",
    "MerchantWindow",
    "SpellCast",
    "SpellEffect",
    "RegionHandoff",
    "RegionChanged",
    "LoggedOut",
    "SpellInterrupted",
    "PlayerDied",
    "PlayerRevived",
    "CombatAnimation",
    "UpdateIcons",
    "ConcentrationList",
    "CharacterPoints",
    "PetWindow",
    "SkillsPage",
    "ObjectRemoved",
    "RemoveObject",
    "TargetChanged",
    "UdpInitReply",
    "BadNameCheckReply",
    "DupNameCheckReply",
    "CharacterCreateReply",
    "CheckLosRequest",
    "TimerWindow",
    "DisableSkills",
    "PlaySound",
    "SoundEffect",
    "ModelChange",
    "MovingObjectCreate",
    "ObjectDataUpdate",
    "Riding",
    "PlayerModelTypeChange",
    "DelveInfo",
    "ControlledHorse",
    "PlayerPosition",
    "ConsignmentMerchantMoney",
    "ObjectGuildId",
    "Encumberance",
    "TrainerWindow",
    "MarketExplorer",
    "EmblemDialogue",
    "FindGroupUpdate",
];

/// Events copied onto [`caer_render::live::Drain`] fields (not merely `WorldState::apply`).
pub const PRODUCT_CONSUMER_EVENTS: &[&str] = &[
    "PlayerPosition",
    "ChatMessage",
    "CombatAnimation",
    "SpellCast",
    "SpellEffect",
    "CharacterOverview",
    "RegionChanged",
    "ObjectRemoved",
    "LoggedOut",
    "Dialog",
    "TradeWindow",
    "GroupWindow",
    "GroupMemberUpdate",
    "QuestEntry",
    "InventoryUpdated",
    "MoneyUpdated",
    "CryptKeyReceived",
    "LoginGranted",
    "Realm",
    "LoginDenied",
    "AttackMode",
    "MaxSpeed",
    "TargetChanged",
    "UdpInitReply",
    "CheckLosRequest",
    "BadNameCheckReply",
    "DupNameCheckReply",
    "CharacterCreateReply",
    "DelveInfo",
    "RemoveObject",
    "RegionHandoff",
    "EnteredWorld",
];

/// `(event_name, scenario_id, layer)`. Layer is never `PLAYER_SCENARIO`.
pub const SCENARIO_PROOF: &[(&str, &str, &str)] = &[
    ("LoginGranted", "SCN-01", "PlayScenario"),
    ("CharacterOverview", "SCN-01", "PlayScenario"),
    ("EnteredWorld", "SCN-01", "PlayScenario"),
    ("CharacterCreateReply", "SCN-02", "PlayScenario"),
    ("CombatAnimation", "SCN-05", "PlayScenario"),
    ("InventoryUpdated", "SCN-06", "PlayScenario"),
    ("MoneyUpdated", "SCN-06", "PlayScenario"),
    ("EquipmentUpdated", "SCN-06", "PlayScenario"),
    ("SpellCast", "SCN-07", "PlayScenario"),
    ("SpellEffect", "SCN-07", "PlayScenario"),
    ("SpellInterrupted", "SCN-07", "PlayScenario"),
    ("MerchantWindow", "SCN-08", "PlayScenario"),
    ("PlayerDied", "SCN-09", "PlayScenario"),
    ("PlayerRevived", "SCN-09", "PlayScenario"),
];

#[must_use]
pub fn is_state_apply(event: &str) -> bool {
    STATE_APPLY_EVENTS.contains(&event)
}

#[must_use]
pub fn is_product_consumer(event: &str) -> bool {
    PRODUCT_CONSUMER_EVENTS.contains(&event)
}

#[must_use]
pub fn events_for_packet_name(name: &str) -> Vec<&'static str> {
    match name {
        "VariousUpdate" => vec!["SkillsPage", "CharacterSheet", "GroupWindow"],
        "PingReply" => vec![],
        other => TYPED_S2C_ROUTES
            .iter()
            .filter(|(_, code, _)| *code == other)
            .map(|(_, _, event)| *event)
            .collect(),
    }
}

#[must_use]
pub fn stage_flags(packet_name: &str) -> (bool, bool, &'static str) {
    let events = events_for_packet_name(packet_name);
    let state = events.iter().any(|e| is_state_apply(e));
    let product = events.iter().any(|e| is_product_consumer(e));
    let mut proof = "-";
    for e in &events {
        if let Some((_, id, _)) = SCENARIO_PROOF.iter().find(|(ev, _, _)| ev == e) {
            proof = *id;
            break;
        }
    }
    (state, product, proof)
}

#[must_use]
pub fn highest_stage(
    oracle: bool,
    typed: bool,
    state: bool,
    product: bool,
    scenario: &str,
) -> &'static str {
    if scenario != "-" {
        return "SCENARIO_PROOF";
    }
    if product {
        return "PRODUCT_CONSUMER";
    }
    if state {
        return "STATE_APPLY";
    }
    if typed {
        return "TYPED_DECODE";
    }
    if oracle {
        return "ORACLE";
    }
    "NONE"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_decode_is_not_automatically_product() {
        assert!(events_for_packet_name("TrainerWindow")
            .iter()
            .any(|e| *e == "TrainerWindow"));
        assert!(is_state_apply("TrainerWindow"));
        assert!(
            !is_product_consumer("TrainerWindow"),
            "TrainerWindow is typed+eco, not Drain/product chrome"
        );
    }

    #[test]
    fn scenario_proof_is_never_player_scenario() {
        for (_, _, layer) in SCENARIO_PROOF {
            assert_ne!(*layer, "PLAYER_SCENARIO");
        }
    }

    #[test]
    fn product_is_subset_of_named_events() {
        for e in PRODUCT_CONSUMER_EVENTS {
            assert!(
                STATE_APPLY_EVENTS.contains(e)
                    || matches!(
                        *e,
                        "ChatMessage"
                            | "Dialog"
                            | "TradeWindow"
                            | "GroupWindow"
                            | "GroupMemberUpdate"
                            | "QuestEntry"
                            | "CryptKeyReceived"
                            | "LoginGranted"
                            | "Realm"
                            | "LoginDenied"
                            | "MaxSpeed"
                            | "CharacterOverview"
                            | "EnteredWorld"
                    ),
                "{e} product with no state and not a listed drain-only event"
            );
        }
    }
}
