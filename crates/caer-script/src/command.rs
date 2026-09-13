//! Addon-visible command identifiers and categories.
//!
//! Commands are what addons *request* of the client (future host will gate these by capability).
//! They align with today's `LiveCommand` where a counterpart exists; provisional entries reserve
//! stable names for group/zone verbs the host does not yet dispatch.

use core::fmt;

/// Coarse bucket for addon command docs and `caer addon check`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddonCommandCategory {
    Combat,
    Spell,
    Inventory,
    Chat,
    Zone,
    Group,
    Movement,
    Session,
}

impl AddonCommandCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Combat => "combat",
            Self::Spell => "spell",
            Self::Inventory => "inventory",
            Self::Chat => "chat",
            Self::Zone => "zone",
            Self::Group => "group",
            Self::Movement => "movement",
            Self::Session => "session",
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
            "movement" => Self::Movement,
            "session" => Self::Session,
            _ => return None,
        })
    }
}

impl fmt::Display for AddonCommandCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Stable addon-facing command names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddonCommand {
    // --- combat ---
    Attack,
    Target,

    // --- spell / skill ---
    UseSkill,

    // --- inventory ---
    MoveItem,
    Interact,
    BuyItem,

    // --- chat ---
    Say,
    SlashCommand,

    // --- zone (provisional host gaps reserved as stable names) ---
    SetGroundTarget,
    UseDoor,

    // --- group (provisional) ---
    InviteGroup,
    LeaveGroup,
    TradeOffer,

    // --- movement ---
    Move,

    // --- session ---
    Quit,
    SelectCharacter,
    CreateCharacter,
    RequestCharacterOverview,
    RequestNpc,
}

impl AddonCommand {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Attack => "combat.attack",
            Self::Target => "combat.target",
            Self::UseSkill => "spell.use_skill",
            Self::MoveItem => "inventory.move_item",
            Self::Interact => "inventory.interact",
            Self::BuyItem => "inventory.buy_item",
            Self::Say => "chat.say",
            Self::SlashCommand => "chat.slash_command",
            Self::SetGroundTarget => "zone.set_ground_target",
            Self::UseDoor => "zone.use_door",
            Self::InviteGroup => "group.invite",
            Self::LeaveGroup => "group.leave",
            Self::TradeOffer => "group.trade_offer",
            Self::Move => "movement.move",
            Self::Quit => "session.quit",
            Self::SelectCharacter => "session.select_character",
            Self::CreateCharacter => "session.create_character",
            Self::RequestCharacterOverview => "session.request_character_overview",
            Self::RequestNpc => "session.request_npc",
        }
    }

    pub const fn category(self) -> AddonCommandCategory {
        match self {
            Self::Attack | Self::Target => AddonCommandCategory::Combat,
            Self::UseSkill => AddonCommandCategory::Spell,
            Self::MoveItem | Self::Interact | Self::BuyItem => AddonCommandCategory::Inventory,
            Self::Say | Self::SlashCommand => AddonCommandCategory::Chat,
            Self::SetGroundTarget | Self::UseDoor => AddonCommandCategory::Zone,
            Self::InviteGroup | Self::LeaveGroup | Self::TradeOffer => AddonCommandCategory::Group,
            Self::Move => AddonCommandCategory::Movement,
            Self::Quit
            | Self::SelectCharacter
            | Self::CreateCharacter
            | Self::RequestCharacterOverview
            | Self::RequestNpc => AddonCommandCategory::Session,
        }
    }

    pub fn all() -> &'static [AddonCommand] {
        &ALL_COMMANDS
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::all().iter().copied().find(|c| c.as_str() == s)
    }
}

impl fmt::Display for AddonCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

const ALL_COMMANDS: [AddonCommand; 19] = [
    AddonCommand::Attack,
    AddonCommand::Target,
    AddonCommand::UseSkill,
    AddonCommand::MoveItem,
    AddonCommand::Interact,
    AddonCommand::BuyItem,
    AddonCommand::Say,
    AddonCommand::SlashCommand,
    AddonCommand::SetGroundTarget,
    AddonCommand::UseDoor,
    AddonCommand::InviteGroup,
    AddonCommand::LeaveGroup,
    AddonCommand::TradeOffer,
    AddonCommand::Move,
    AddonCommand::Quit,
    AddonCommand::SelectCharacter,
    AddonCommand::CreateCharacter,
    AddonCommand::RequestCharacterOverview,
    AddonCommand::RequestNpc,
];
