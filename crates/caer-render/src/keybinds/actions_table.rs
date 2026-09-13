//! The authoritative **74-action** keyboard table extracted from `game1127.dll`.
//!
//! Source: `data/client_tables/keyboard_actions_internal.tsv` (leg 15). Indices and
//! `internal_name` values are the client's own strings — not invented here.
//!
//! ## What "binding" means in this table
//!
//! DAoC's `keyboard.dat` stores one `(key, modifier)` pair per **slot**. An action may later gain
//! extra keys through rebinding, and CAER also keeps a few secondary aliases (e.g. arrow keys for
//! forward/back) outside the primary row. The **primary** chord is the single default identity for
//! each of the 74 actions: every action has exactly one, and no two actions share one.
//!
//! Slot→action index mapping for `keyboard.dat` is still unpinned (`docs/RESUME_HERE.md`); primary
//! chords here are provisional CAER defaults chosen for uniqueness and known DAoC muscle memory
//! (W/S move, Q/E slide, Tab nearest enemy, Enter chat, F combat, Alt+Z interface, F1–F8 group).

use winit::keyboard::KeyCode;

use super::{Chord, GameAction};

/// One row of the DLL action table plus its CAER primary default chord.
#[derive(Clone, Copy, Debug)]
pub struct ActionTableEntry {
    /// Index in `keyboard_actions_internal.tsv` (0..73).
    pub index: u8,
    /// Exact internal string from the DLL string table.
    pub internal_name: &'static str,
    pub action: GameAction,
    /// Stable config-file name (`snake_case`).
    pub config_name: &'static str,
    /// Exactly one primary default chord; unique across the whole table.
    pub primary: Chord,
}

/// Number of actions in the extracted internal table. A test fails if this drifts.
pub const TABLE_ACTION_COUNT: usize = 74;

/// The full 74-action table. Order is table-index order.
pub static ACTIONS_TABLE: [ActionTableEntry; TABLE_ACTION_COUNT] = [
    entry(
        0,
        "Forward",
        GameAction::Forward,
        "forward",
        plain(KeyCode::KeyW),
    ),
    entry(1, "Back", GameAction::Back, "back", plain(KeyCode::KeyS)),
    entry(
        2,
        "Jump/Up",
        GameAction::JumpUp,
        "jump_up",
        plain(KeyCode::Space),
    ),
    entry(
        3,
        "Walk",
        GameAction::Walk,
        "walk",
        plain(KeyCode::CapsLock),
    ),
    entry(
        4,
        "Slide Left",
        GameAction::SlideLeft,
        "slide_left",
        plain(KeyCode::KeyQ),
    ),
    entry(
        5,
        "Slide Right",
        GameAction::SlideRight,
        "slide_right",
        plain(KeyCode::KeyE),
    ),
    entry(6, "Open", GameAction::Open, "open", plain(KeyCode::KeyO)),
    entry(
        7,
        "Look up",
        GameAction::LookUp,
        "look_up",
        plain(KeyCode::PageUp),
    ),
    entry(
        8,
        "Look Down",
        GameAction::LookDown,
        "look_down",
        plain(KeyCode::PageDown),
    ),
    entry(
        9,
        "Reset Camera",
        GameAction::ResetCamera,
        "reset_camera",
        plain(KeyCode::Home),
    ),
    // Mouse-button actions have no KeyCode; provisional keyboard stand-ins until mouse chords exist.
    entry(
        10,
        "Mouse",
        GameAction::Mouse,
        "mouse",
        plain(KeyCode::Insert),
    ),
    entry(
        11,
        "Toggle Names",
        GameAction::ToggleNames,
        "toggle_names",
        alt(KeyCode::KeyN),
    ),
    entry(
        12,
        "Forward2",
        GameAction::Forward2,
        "forward2",
        plain(KeyCode::Numpad8),
    ),
    entry(13, "Get", GameAction::Get, "get", plain(KeyCode::KeyG)),
    entry(
        14,
        "Torch",
        GameAction::Torch,
        "torch",
        plain(KeyCode::KeyL),
    ),
    entry(15, "Chat", GameAction::Chat, "chat", plain(KeyCode::Enter)),
    entry(
        16,
        "Target Enemy",
        GameAction::TargetEnemy,
        "target_enemy",
        plain(KeyCode::Tab),
    ),
    entry(
        17,
        "Target Friend",
        GameAction::TargetFriend,
        "target_friend",
        plain(KeyCode::Backquote),
    ),
    entry(
        18,
        "Target Object",
        GameAction::TargetObject,
        "target_object",
        ctrl(KeyCode::Tab),
    ),
    entry(
        19,
        "Combat Mode",
        GameAction::CombatMode,
        "combat_mode",
        plain(KeyCode::KeyF),
    ),
    entry(
        20,
        "Show Spells",
        GameAction::ShowSpells,
        "show_spells",
        plain(KeyCode::KeyP),
    ),
    entry(21, "Sell", GameAction::Sell, "sell", plain(KeyCode::KeyK)),
    entry(
        22,
        "Stats",
        GameAction::Stats,
        "stats",
        plain(KeyCode::KeyC),
    ),
    entry(
        23,
        "Inventory",
        GameAction::Inventory,
        "inventory",
        plain(KeyCode::KeyI),
    ),
    entry(
        24,
        "Group",
        GameAction::Group,
        "group",
        plain(KeyCode::KeyY),
    ),
    entry(
        25,
        "Back2",
        GameAction::Back2,
        "back2",
        plain(KeyCode::Numpad2),
    ),
    entry(
        26,
        "Use Item",
        GameAction::UseItem,
        "use_item",
        plain(KeyCode::KeyV),
    ),
    entry(
        27,
        "Last Attacker",
        GameAction::LastAttacker,
        "last_attacker",
        plain(KeyCode::KeyZ),
    ),
    entry(
        28,
        "Reply",
        GameAction::Reply,
        "reply",
        plain(KeyCode::KeyR),
    ),
    entry(
        29,
        "ChatLog",
        GameAction::ChatLog,
        "chat_log",
        ctrl(KeyCode::KeyL),
    ),
    entry(
        30,
        "Destroy",
        GameAction::Destroy,
        "destroy",
        plain(KeyCode::Delete),
    ),
    entry(
        31,
        "Mouse Look Toggle",
        GameAction::MouseLookToggle,
        "mouse_look_toggle",
        plain(KeyCode::KeyM),
    ),
    entry(
        32,
        "Show Skills",
        GameAction::ShowSkills,
        "show_skills",
        plain(KeyCode::KeyB),
    ),
    entry(
        33,
        "Show Combat",
        GameAction::ShowCombat,
        "show_combat",
        shift(KeyCode::KeyB),
    ),
    entry(
        34,
        "Runlock",
        GameAction::Runlock,
        "runlock",
        plain(KeyCode::Semicolon),
    ),
    entry(
        35,
        "Consider",
        GameAction::Consider,
        "consider",
        plain(KeyCode::Quote),
    ),
    entry(
        36,
        "Ground Target",
        GameAction::GroundTarget,
        "ground_target",
        ctrl(KeyCode::KeyG),
    ),
    entry(
        37,
        "Target Group1",
        GameAction::TargetGroup1,
        "target_group_1",
        plain(KeyCode::F1),
    ),
    entry(
        38,
        "Target Group2",
        GameAction::TargetGroup2,
        "target_group_2",
        plain(KeyCode::F2),
    ),
    entry(
        39,
        "Target Group3",
        GameAction::TargetGroup3,
        "target_group_3",
        plain(KeyCode::F4),
    ),
    entry(
        40,
        "Target Group4",
        GameAction::TargetGroup4,
        "target_group_4",
        plain(KeyCode::F5),
    ),
    entry(
        41,
        "Target Group5",
        GameAction::TargetGroup5,
        "target_group_5",
        plain(KeyCode::F6),
    ),
    entry(
        42,
        "Target Group6",
        GameAction::TargetGroup6,
        "target_group_6",
        plain(KeyCode::F7),
    ),
    entry(
        43,
        "Target Group7",
        GameAction::TargetGroup7,
        "target_group_7",
        plain(KeyCode::F8),
    ),
    entry(
        44,
        "Target Group8",
        GameAction::TargetGroup8,
        "target_group_8",
        plain(KeyCode::F9),
    ),
    entry(
        45,
        "PageUp",
        GameAction::PageUp,
        "page_up",
        ctrl(KeyCode::PageUp),
    ),
    entry(
        46,
        "PageDown",
        GameAction::PageDown,
        "page_down",
        ctrl(KeyCode::PageDown),
    ),
    entry(
        47,
        "PageUpSys",
        GameAction::PageUpSys,
        "page_up_sys",
        alt(KeyCode::PageUp),
    ),
    entry(
        48,
        "PageDownSys",
        GameAction::PageDownSys,
        "page_down_sys",
        alt(KeyCode::PageDown),
    ),
    entry(49, "Sit", GameAction::Sit, "sit", plain(KeyCode::KeyX)),
    entry(
        50,
        "Perf. Meter",
        GameAction::PerfMeter,
        "perf_meter",
        plain(KeyCode::F11),
    ),
    entry(
        51,
        "Compass",
        GameAction::Compass,
        "compass",
        plain(KeyCode::KeyN),
    ),
    entry(
        52,
        "Information/Delve",
        GameAction::InformationDelve,
        "information_delve",
        shift(KeyCode::KeyI),
    ),
    entry(
        53,
        "Pan Camera",
        GameAction::PanCamera,
        "pan_camera",
        plain(KeyCode::Comma),
    ),
    entry(
        54,
        "Follow",
        GameAction::Follow,
        "follow",
        ctrl(KeyCode::KeyF),
    ),
    entry(55, "Stick", GameAction::Stick, "stick", ctrl(KeyCode::KeyS)),
    entry(56, "Face", GameAction::Face, "face", ctrl(KeyCode::KeyA)),
    entry(
        57,
        "Craft",
        GameAction::Craft,
        "craft",
        plain(KeyCode::Period),
    ),
    entry(
        58,
        "Sprint",
        GameAction::Sprint,
        "sprint",
        plain(KeyCode::Equal),
    ),
    entry(
        59,
        "Command Window",
        GameAction::CommandWindow,
        "command_window",
        plain(KeyCode::F10),
    ),
    entry(
        60,
        "Right Hand Weapon",
        GameAction::RightHandWeapon,
        "right_hand_weapon",
        ctrl(KeyCode::Digit1),
    ),
    entry(
        61,
        "Two Handed Weapon",
        GameAction::TwoHandedWeapon,
        "two_handed_weapon",
        ctrl(KeyCode::Digit2),
    ),
    entry(
        62,
        "Ranged Weapon",
        GameAction::RangedWeapon,
        "ranged_weapon",
        ctrl(KeyCode::Digit3),
    ),
    entry(
        63,
        "Use Item Secondary",
        GameAction::UseItemSecondary,
        "use_item_secondary",
        plain(KeyCode::BracketLeft),
    ),
    entry(
        64,
        "Nearest Loot",
        GameAction::NearestLoot,
        "nearest_loot",
        plain(KeyCode::Backslash),
    ),
    entry(
        65,
        "Mouse Pan",
        GameAction::MousePan,
        "mouse_pan",
        plain(KeyCode::BracketRight),
    ),
    entry(
        66,
        "Map Window",
        GameAction::MapWindow,
        "map_window",
        alt(KeyCode::KeyM),
    ),
    entry(
        67,
        "Toggle Interface",
        GameAction::ToggleInterface,
        "toggle_interface",
        alt(KeyCode::KeyZ),
    ),
    entry(
        68,
        "Realm War Map",
        GameAction::RealmWarMap,
        "realm_war_map",
        ctrl(KeyCode::KeyM),
    ),
    entry(
        69,
        "Quest Journal",
        GameAction::QuestJournal,
        "quest_journal",
        plain(KeyCode::KeyJ),
    ),
    entry(
        70,
        "Run Lock 2",
        GameAction::RunLock2,
        "run_lock_2",
        shift(KeyCode::Semicolon),
    ),
    entry(
        71,
        "Show Character Info",
        GameAction::ShowCharacterInfo,
        "show_character_info",
        shift(KeyCode::KeyC),
    ),
    entry(
        72,
        "Take Screenshot",
        GameAction::TakeScreenshot,
        "take_screenshot",
        plain(KeyCode::F12),
    ),
    entry(
        73,
        "Camera Toggle",
        GameAction::CameraToggle,
        "camera_toggle",
        plain(KeyCode::F3),
    ),
];

const fn entry(
    index: u8,
    internal_name: &'static str,
    action: GameAction,
    config_name: &'static str,
    primary: Chord,
) -> ActionTableEntry {
    ActionTableEntry {
        index,
        internal_name,
        action,
        config_name,
        primary,
    }
}

const fn plain(key: KeyCode) -> Chord {
    Chord {
        key,
        ctrl: false,
        alt: false,
        shift: false,
    }
}

const fn alt(key: KeyCode) -> Chord {
    Chord {
        key,
        ctrl: false,
        alt: true,
        shift: false,
    }
}

const fn ctrl(key: KeyCode) -> Chord {
    Chord {
        key,
        ctrl: true,
        alt: false,
        shift: false,
    }
}

const fn shift(key: KeyCode) -> Chord {
    Chord {
        key,
        ctrl: false,
        alt: false,
        shift: true,
    }
}

/// Look up a table row by action. Every `GameAction` is in the table exactly once.
#[must_use]
pub fn entry_for(action: GameAction) -> Option<&'static ActionTableEntry> {
    ACTIONS_TABLE.iter().find(|e| e.action == action)
}
