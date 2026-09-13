//! Introspectable catalog of every addon event and command.
//!
//! The catalog is the source of truth for `caer addon check` and for future host bindings.
//! Each row carries optional provenance to today's wire/session types so the Lua surface can
//! stay stable while decoders and `LiveCommand` variants move.

use crate::command::{AddonCommand, AddonCommandCategory};
use crate::event::{AddonEvent, AddonEventCategory};

/// Whether the host already has a dispatch path, or the name is reserved ahead of implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stability {
    /// Backed by an existing `ServerEvent` / `LiveCommand` (or equivalent) path today.
    Stable,
    /// Name reserved so the API does not freeze around a temporary gap; host not yet wired.
    Provisional,
}

impl Stability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Provisional => "provisional",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CatalogKind {
    Event,
    Command,
}

impl CatalogKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Command => "command",
        }
    }
}

/// One row in the addon taxonomy catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddonCatalogEntry {
    pub id: &'static str,
    pub kind: CatalogKind,
    pub event_category: Option<AddonEventCategory>,
    pub command_category: Option<AddonCommandCategory>,
    pub stability: Stability,
    /// Human-readable pointer to today's counterpart, e.g. `ServerEvent::CombatAnimation`.
    pub provenance: Option<&'static str>,
    pub summary: &'static str,
}

impl AddonCatalogEntry {
    pub const fn for_event(
        event: AddonEvent,
        stability: Stability,
        provenance: Option<&'static str>,
        summary: &'static str,
    ) -> Self {
        Self {
            id: event.as_str(),
            kind: CatalogKind::Event,
            event_category: Some(event.category()),
            command_category: None,
            stability,
            provenance,
            summary,
        }
    }

    pub const fn for_command(
        command: AddonCommand,
        stability: Stability,
        provenance: Option<&'static str>,
        summary: &'static str,
    ) -> Self {
        Self {
            id: command.as_str(),
            kind: CatalogKind::Command,
            event_category: None,
            command_category: Some(command.category()),
            stability,
            provenance,
            summary,
        }
    }
}

/// Full taxonomy table. Keep in sync with [`AddonEvent`] / [`AddonCommand`] variants.
pub static CATALOG: &[AddonCatalogEntry] = &[
    // ---- events: combat ----
    AddonCatalogEntry::for_event(
        AddonEvent::CombatSwing,
        Stability::Stable,
        Some("ServerEvent::CombatAnimation"),
        "Structured combat swing feedback (result + target HP%).",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::PlayerDied,
        Stability::Provisional,
        Some("ServerEvent::PlayerDied"),
        "Reserved: rustdaoc Drain does not yet emit combat.player_died.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::PlayerRevived,
        Stability::Provisional,
        Some("ServerEvent::PlayerRevived"),
        "Reserved: rustdaoc Drain does not yet emit combat.player_revived.",
    ),
    // ---- events: spell ----
    AddonCatalogEntry::for_event(
        AddonEvent::SpellCastStarted,
        Stability::Stable,
        Some("ServerEvent::SpellCast"),
        "Spell cast wind-up began.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::SpellEffectApplied,
        Stability::Stable,
        Some("ServerEvent::SpellEffect"),
        "Spell effect / bolt impact.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::SpellInterrupted,
        Stability::Provisional,
        Some("ServerEvent::SpellInterrupted"),
        "Reserved: rustdaoc Drain does not yet emit spell.interrupted.",
    ),
    // ---- events: inventory ----
    AddonCatalogEntry::for_event(
        AddonEvent::InventoryUpdated,
        Stability::Stable,
        Some("ServerEvent::InventoryUpdated"),
        "Local bag / worn / vault slot contents changed.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::MoneyUpdated,
        Stability::Stable,
        Some("ServerEvent::MoneyUpdated"),
        "Local purse changed.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::EquipmentUpdated,
        Stability::Provisional,
        Some("ServerEvent::EquipmentUpdated"),
        "Reserved: rustdaoc Drain does not yet emit inventory.equipment_updated.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::MerchantWindowOpened,
        Stability::Provisional,
        Some("ServerEvent::MerchantWindow"),
        "Reserved: rustdaoc Drain does not yet emit inventory.merchant_window.",
    ),
    // ---- events: chat ----
    AddonCatalogEntry::for_event(
        AddonEvent::ChatMessage,
        Stability::Stable,
        Some("ServerEvent::ChatMessage"),
        "Chat / system message.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::DialogOpened,
        Stability::Stable,
        Some("ServerEvent::Dialog"),
        "Non-invite dialog box (quest subscribe, custom dialog, etc.).",
    ),
    // ---- events: zone ----
    AddonCatalogEntry::for_event(
        AddonEvent::EnteredWorld,
        Stability::Stable,
        Some("ServerEvent::EnteredWorld"),
        "Fully in-world after region handoff.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::RegionChanged,
        Stability::Stable,
        Some("ServerEvent::RegionChanged"),
        "In-world region transition.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::CharacterJumped,
        Stability::Provisional,
        Some("ServerEvent::CharacterJump"),
        "Reserved: rustdaoc Drain does not yet emit zone.character_jumped.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::GroundTargetChanged,
        Stability::Provisional,
        Some("ServerEvent::GroundTargetChanged"),
        "Reserved: rustdaoc Drain does not yet emit zone.ground_target_changed.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::DoorStateChanged,
        Stability::Provisional,
        Some("ServerEvent::DoorState"),
        "Reserved: rustdaoc Drain does not yet emit zone.door_state_changed.",
    ),
    // ---- events: group ----
    AddonCatalogEntry::for_event(
        AddonEvent::GroupMemberUpdated,
        Stability::Stable,
        Some("ServerEvent::GroupMemberUpdate"),
        "Group roster vitals update.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::GroupWindowUpdated,
        Stability::Stable,
        Some("ServerEvent::GroupWindow"),
        "Group window names / clear (empty = left/disbanded).",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::TradeWindowUpdated,
        Stability::Stable,
        Some("ServerEvent::TradeWindow"),
        "Player trade window open/update/close.",
    ),
    // ---- events: session ----
    AddonCatalogEntry::for_event(
        AddonEvent::CryptKeyReceived,
        Stability::Stable,
        Some("ServerEvent::CryptKeyReceived"),
        "Crypt handshake completed.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::LoginGranted,
        Stability::Stable,
        Some("ServerEvent::LoginGranted"),
        "Login granted; character select follows.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::CharacterOverviewReady,
        Stability::Stable,
        Some("ServerEvent::CharacterOverview"),
        "Character-select overview decoded.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::LoggedOut,
        Stability::Stable,
        Some("ServerEvent::LoggedOut"),
        "Server confirmed clean logout.",
    ),
    // ---- events: entity ----
    AddonCatalogEntry::for_event(
        AddonEvent::NpcInView,
        Stability::Provisional,
        Some("ServerEvent::NpcInView"),
        "Reserved: rustdaoc Drain does not yet emit entity.npc_in_view.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::PlayerInView,
        Stability::Provisional,
        Some("ServerEvent::PlayerInView"),
        "Reserved: rustdaoc Drain does not yet emit entity.player_in_view.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::ObjectInView,
        Stability::Provisional,
        Some("ServerEvent::ObjectInView"),
        "Reserved: rustdaoc Drain does not yet emit entity.object_in_view.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::EntityUpdated,
        Stability::Provisional,
        Some("ServerEvent::EntityUpdated"),
        "Reserved: rustdaoc Drain does not yet emit entity.updated.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::ObjectRemoved,
        Stability::Stable,
        Some("ServerEvent::ObjectRemoved"),
        "Entity left view and must be culled.",
    ),
    // ---- events: status ----
    AddonCatalogEntry::for_event(
        AddonEvent::StatusUpdated,
        Stability::Provisional,
        Some("ServerEvent::StatusUpdate"),
        "Reserved: rustdaoc Drain does not yet emit status.vitals_updated.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::SkillsPageUpdated,
        Stability::Provisional,
        Some("ServerEvent::SkillsPage"),
        "Reserved: rustdaoc Drain does not yet emit status.skills_page.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::CharacterSheetUpdated,
        Stability::Provisional,
        Some("ServerEvent::CharacterSheet"),
        "Reserved: rustdaoc Drain does not yet emit status.character_sheet.",
    ),
    AddonCatalogEntry::for_event(
        AddonEvent::QuestEntryUpdated,
        Stability::Stable,
        Some("ServerEvent::QuestEntry"),
        "Quest log slot update.",
    ),
    // ---- commands: combat ----
    AddonCatalogEntry::for_command(
        AddonCommand::Attack,
        Stability::Stable,
        Some("LiveCommand::Attack"),
        "Start or stop melee attack mode.",
    ),
    AddonCatalogEntry::for_command(
        AddonCommand::Target,
        Stability::Stable,
        Some("LiveCommand::Target"),
        "Set or clear the server-side target.",
    ),
    // ---- commands: spell ----
    AddonCatalogEntry::for_command(
        AddonCommand::UseSkill,
        Stability::Stable,
        Some("LiveCommand::UseSkill"),
        "Use a skill/style/spell by usable-skill index.",
    ),
    // ---- commands: inventory ----
    AddonCatalogEntry::for_command(
        AddonCommand::MoveItem,
        Stability::Stable,
        Some("LiveCommand::MoveItem"),
        "Move an item between inventory slots (equip path).",
    ),
    AddonCatalogEntry::for_command(
        AddonCommand::Interact,
        Stability::Stable,
        Some("LiveCommand::Interact"),
        "Object interact (merchant open / use).",
    ),
    AddonCatalogEntry::for_command(
        AddonCommand::BuyItem,
        Stability::Stable,
        Some("LiveCommand::BuyItem"),
        "Buy from a targeted merchant.",
    ),
    // ---- commands: chat ----
    AddonCatalogEntry::for_command(
        AddonCommand::Say,
        Stability::Stable,
        Some("LiveCommand::Say"),
        "Say a line to nearby players.",
    ),
    AddonCatalogEntry::for_command(
        AddonCommand::SlashCommand,
        Stability::Stable,
        Some("LiveCommand::Command"),
        "Slash command without leading `/`.",
    ),
    // ---- commands: zone (provisional) ----
    AddonCatalogEntry::for_command(
        AddonCommand::SetGroundTarget,
        Stability::Provisional,
        None,
        "Reserve stable name for client→server ground-target set (host not yet wired).",
    ),
    AddonCatalogEntry::for_command(
        AddonCommand::UseDoor,
        Stability::Provisional,
        None,
        "Reserve stable name for door use verb (host not yet wired).",
    ),
    // ---- commands: group (provisional) ----
    AddonCatalogEntry::for_command(
        AddonCommand::InviteGroup,
        Stability::Stable,
        Some("LiveCommand::InviteToGroup"),
        "Group invite (requires a prior target).",
    ),
    AddonCatalogEntry::for_command(
        AddonCommand::LeaveGroup,
        Stability::Provisional,
        None,
        "Reserve stable name for leave/disband (host not yet wired).",
    ),
    AddonCatalogEntry::for_command(
        AddonCommand::TradeOffer,
        Stability::Provisional,
        None,
        "Reserve stable name for trade offer/accept (host not yet wired).",
    ),
    // ---- commands: movement ----
    AddonCatalogEntry::for_command(
        AddonCommand::Move,
        Stability::Provisional,
        Some("LiveCommand::Move"),
        "Reserved: rustdaoc will not invent pose/motion fields for movement.move.",
    ),
    // ---- commands: session ----
    AddonCatalogEntry::for_command(
        AddonCommand::Quit,
        Stability::Stable,
        Some("LiveCommand::Quit"),
        "Request clean logout.",
    ),
    AddonCatalogEntry::for_command(
        AddonCommand::SelectCharacter,
        Stability::Stable,
        Some("LiveCommand::SelectCharacter"),
        "Choose a character by realm-local slot.",
    ),
    AddonCatalogEntry::for_command(
        AddonCommand::CreateCharacter,
        Stability::Provisional,
        Some("LiveCommand::CreateCharacter"),
        "Reserved: rustdaoc will not invent a CharacterCreateDraft from sparse Lua args.",
    ),
    AddonCatalogEntry::for_command(
        AddonCommand::RequestCharacterOverview,
        Stability::Stable,
        Some("LiveCommand::RequestCharacterOverview"),
        "Re-request character overview for a realm.",
    ),
    AddonCatalogEntry::for_command(
        AddonCommand::RequestNpc,
        Stability::Stable,
        Some("LiveCommand::RequestNpc"),
        "Ask the server to resend an entity create packet.",
    ),
];

/// Convenience iterator over [`CATALOG`].
pub fn catalog_entries() -> &'static [AddonCatalogEntry] {
    CATALOG
}
