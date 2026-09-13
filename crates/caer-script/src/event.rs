//! Addon-visible event identifiers and categories.
//!
//! Categories are the acceptance surface for `caer addon check`. Individual events may grow;
//! required categories must not disappear.

use core::fmt;

/// Coarse bucket an addon author filters / documents against.
///
/// The six values marked required by [`crate::REQUIRED_EVENT_CATEGORIES`] are the LUA-SEQ
/// acceptance floor (combat, spell, inventory, chat, zone/region, group). Additional categories
/// are allowed; they do not relax the floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddonEventCategory {
    Combat,
    Spell,
    Inventory,
    Chat,
    /// Zone / region transitions and world-placement confirms.
    Zone,
    Group,
    /// Session / login / character-select lifecycle (not in the required floor).
    Session,
    /// Entity visibility stream (not in the required floor).
    Entity,
    /// Local vitals / sheet (not in the required floor).
    Status,
}

impl AddonEventCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Combat => "combat",
            Self::Spell => "spell",
            Self::Inventory => "inventory",
            Self::Chat => "chat",
            Self::Zone => "zone",
            Self::Group => "group",
            Self::Session => "session",
            Self::Entity => "entity",
            Self::Status => "status",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "combat" => Self::Combat,
            "spell" => Self::Spell,
            "inventory" => Self::Inventory,
            "chat" => Self::Chat,
            "zone" | "region" | "zone/region" => Self::Zone,
            "group" => Self::Group,
            "session" => Self::Session,
            "entity" => Self::Entity,
            "status" => Self::Status,
            _ => return None,
        })
    }
}

impl fmt::Display for AddonEventCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Stable addon-facing event names.
///
/// These are **not** a dump of every `ServerEvent` variant. Temporary or transport-only events
/// stay off this list until an external participant (player / addon / protocol peer / persisted
/// data) can observe or depend on them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AddonEvent {
    // --- combat ---
    CombatSwing,
    PlayerDied,
    PlayerRevived,

    // --- spell ---
    SpellCastStarted,
    SpellEffectApplied,
    SpellInterrupted,

    // --- inventory ---
    InventoryUpdated,
    MoneyUpdated,
    EquipmentUpdated,
    MerchantWindowOpened,

    // --- chat ---
    ChatMessage,
    DialogOpened,

    // --- zone / region ---
    EnteredWorld,
    RegionChanged,
    CharacterJumped,
    GroundTargetChanged,
    DoorStateChanged,

    // --- group ---
    GroupMemberUpdated,
    GroupWindowUpdated,
    TradeWindowUpdated,

    // --- session (extra) ---
    CryptKeyReceived,
    LoginGranted,
    CharacterOverviewReady,
    LoggedOut,

    // --- entity (extra) ---
    NpcInView,
    PlayerInView,
    ObjectInView,
    EntityUpdated,
    ObjectRemoved,

    // --- status (extra) ---
    StatusUpdated,
    SkillsPageUpdated,
    CharacterSheetUpdated,
    QuestEntryUpdated,
}

impl AddonEvent {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CombatSwing => "combat.swing",
            Self::PlayerDied => "combat.player_died",
            Self::PlayerRevived => "combat.player_revived",
            Self::SpellCastStarted => "spell.cast_started",
            Self::SpellEffectApplied => "spell.effect_applied",
            Self::SpellInterrupted => "spell.interrupted",
            Self::InventoryUpdated => "inventory.updated",
            Self::MoneyUpdated => "inventory.money_updated",
            Self::EquipmentUpdated => "inventory.equipment_updated",
            Self::MerchantWindowOpened => "inventory.merchant_window",
            Self::ChatMessage => "chat.message",
            Self::DialogOpened => "chat.dialog",
            Self::EnteredWorld => "zone.entered_world",
            Self::RegionChanged => "zone.region_changed",
            Self::CharacterJumped => "zone.character_jumped",
            Self::GroundTargetChanged => "zone.ground_target_changed",
            Self::DoorStateChanged => "zone.door_state_changed",
            Self::GroupMemberUpdated => "group.member_updated",
            Self::GroupWindowUpdated => "group.window_updated",
            Self::TradeWindowUpdated => "group.trade_window_updated",
            Self::CryptKeyReceived => "session.crypt_key_received",
            Self::LoginGranted => "session.login_granted",
            Self::CharacterOverviewReady => "session.character_overview",
            Self::LoggedOut => "session.logged_out",
            Self::NpcInView => "entity.npc_in_view",
            Self::PlayerInView => "entity.player_in_view",
            Self::ObjectInView => "entity.object_in_view",
            Self::EntityUpdated => "entity.updated",
            Self::ObjectRemoved => "entity.removed",
            Self::StatusUpdated => "status.vitals_updated",
            Self::SkillsPageUpdated => "status.skills_page",
            Self::CharacterSheetUpdated => "status.character_sheet",
            Self::QuestEntryUpdated => "status.quest_entry",
        }
    }

    pub const fn category(self) -> AddonEventCategory {
        match self {
            Self::CombatSwing | Self::PlayerDied | Self::PlayerRevived => {
                AddonEventCategory::Combat
            }
            Self::SpellCastStarted | Self::SpellEffectApplied | Self::SpellInterrupted => {
                AddonEventCategory::Spell
            }
            Self::InventoryUpdated
            | Self::MoneyUpdated
            | Self::EquipmentUpdated
            | Self::MerchantWindowOpened => AddonEventCategory::Inventory,
            Self::ChatMessage | Self::DialogOpened => AddonEventCategory::Chat,
            Self::EnteredWorld
            | Self::RegionChanged
            | Self::CharacterJumped
            | Self::GroundTargetChanged
            | Self::DoorStateChanged => AddonEventCategory::Zone,
            Self::GroupMemberUpdated | Self::GroupWindowUpdated | Self::TradeWindowUpdated => {
                AddonEventCategory::Group
            }
            Self::CryptKeyReceived
            | Self::LoginGranted
            | Self::CharacterOverviewReady
            | Self::LoggedOut => AddonEventCategory::Session,
            Self::NpcInView
            | Self::PlayerInView
            | Self::ObjectInView
            | Self::EntityUpdated
            | Self::ObjectRemoved => AddonEventCategory::Entity,
            Self::StatusUpdated
            | Self::SkillsPageUpdated
            | Self::CharacterSheetUpdated
            | Self::QuestEntryUpdated => AddonEventCategory::Status,
        }
    }

    /// All known events, in catalog order.
    pub fn all() -> &'static [AddonEvent] {
        &ALL_EVENTS
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::all().iter().copied().find(|e| e.as_str() == s)
    }
}

impl fmt::Display for AddonEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

const ALL_EVENTS: [AddonEvent; 33] = [
    AddonEvent::CombatSwing,
    AddonEvent::PlayerDied,
    AddonEvent::PlayerRevived,
    AddonEvent::SpellCastStarted,
    AddonEvent::SpellEffectApplied,
    AddonEvent::SpellInterrupted,
    AddonEvent::InventoryUpdated,
    AddonEvent::MoneyUpdated,
    AddonEvent::EquipmentUpdated,
    AddonEvent::MerchantWindowOpened,
    AddonEvent::ChatMessage,
    AddonEvent::DialogOpened,
    AddonEvent::EnteredWorld,
    AddonEvent::RegionChanged,
    AddonEvent::CharacterJumped,
    AddonEvent::GroundTargetChanged,
    AddonEvent::DoorStateChanged,
    AddonEvent::GroupMemberUpdated,
    AddonEvent::GroupWindowUpdated,
    AddonEvent::TradeWindowUpdated,
    AddonEvent::CryptKeyReceived,
    AddonEvent::LoginGranted,
    AddonEvent::CharacterOverviewReady,
    AddonEvent::LoggedOut,
    AddonEvent::NpcInView,
    AddonEvent::PlayerInView,
    AddonEvent::ObjectInView,
    AddonEvent::EntityUpdated,
    AddonEvent::ObjectRemoved,
    AddonEvent::StatusUpdated,
    AddonEvent::SkillsPageUpdated,
    AddonEvent::CharacterSheetUpdated,
    AddonEvent::QuestEntryUpdated,
];
