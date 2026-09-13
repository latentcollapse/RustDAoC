//! Keybindings: what the player is trying to DO, decoupled from which key they pressed.
//!
//! The client's controls started as `match KeyCode { KeyCode::KeyW => … }` arms, which hardcodes
//! the layout into the input handler. That blocks the two things this crate actually needs:
//! `/keyboard` (rebinding at runtime) and modding (a config file someone can edit). Both want the
//! same thing — a named ACTION in the middle, with the physical key as data on one side and the
//! game's response on the other.
//!
//! ## Why our own format rather than the client's `keyboard.dat`
//!
//! The original ships `keyboard.dat`: an INI with three control schemes (`fpsdefault`,
//! `rpgdefault`, `mmodefault`), each holding 90 `keyNN=<code>` / `shiftNN=<modifier>` pairs, where
//! `NN` is an action slot. Reading it is attractive for drop-in fidelity, but two things make it a
//! poor SOURCE of truth: the action ids are positional with no names anywhere in the file (they'd
//! have to be reverse-engineered slot by slot), and several bound values decode to keys no Western
//! keyboard has (VK_KANA, VK_HANJA, VK_MODECHANGE), so the encoding is not plain virtual-key codes
//! either. Importing it is a compatibility feature that can land later against a decoded slot
//! table; it is not something to build the binding system on top of.
//!
//! So actions are NAMED here, and the config is text a human can read and a modder can edit.
//!
//! ## The 74-action table (leg 15)
//!
//! `actions_table` embeds `data/client_tables/keyboard_actions_internal.tsv` — 74 actions with
//! DLL-extracted internal names. Every action has exactly one **primary** default [`Chord`]; an
//! action may also hold secondary chords (W and Up both walk forward). A chord never maps to two
//! actions ([`Bindings`] is keyed by chord).

mod actions_table;
pub mod meta;
pub mod parity;

use std::collections::HashMap;

use winit::keyboard::KeyCode;

pub use actions_table::{entry_for, ActionTableEntry, ACTIONS_TABLE, TABLE_ACTION_COUNT};
pub use meta::{
    aliases_for, canonical_action_token, compact_controls_lines, default_controls_help,
    format_bound_chords, is_product_wired, keyboard_list_lines, player_controls_help, COMPACT_HELP,
    LEGACY_ALIASES, PRODUCT_WIRED,
};
pub use parity::{action_parity, ActionParity};

/// Something the player can ask the character (or the client) to do.
///
/// Variants and order match `keyboard_actions_internal.tsv` indices 0..73. Named rather than
/// positional so a config file is readable (`slide_left = KeyQ` instead of a bare slot number).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum GameAction {
    Forward = 0,
    Back = 1,
    JumpUp = 2,
    Walk = 3,
    SlideLeft = 4,
    SlideRight = 5,
    Open = 6,
    LookUp = 7,
    LookDown = 8,
    ResetCamera = 9,
    Mouse = 10,
    ToggleNames = 11,
    Forward2 = 12,
    Get = 13,
    Torch = 14,
    Chat = 15,
    TargetEnemy = 16,
    TargetFriend = 17,
    TargetObject = 18,
    CombatMode = 19,
    ShowSpells = 20,
    Sell = 21,
    Stats = 22,
    Inventory = 23,
    Group = 24,
    Back2 = 25,
    UseItem = 26,
    LastAttacker = 27,
    Reply = 28,
    ChatLog = 29,
    Destroy = 30,
    MouseLookToggle = 31,
    ShowSkills = 32,
    ShowCombat = 33,
    Runlock = 34,
    Consider = 35,
    GroundTarget = 36,
    TargetGroup1 = 37,
    TargetGroup2 = 38,
    TargetGroup3 = 39,
    TargetGroup4 = 40,
    TargetGroup5 = 41,
    TargetGroup6 = 42,
    TargetGroup7 = 43,
    TargetGroup8 = 44,
    PageUp = 45,
    PageDown = 46,
    PageUpSys = 47,
    PageDownSys = 48,
    Sit = 49,
    PerfMeter = 50,
    Compass = 51,
    InformationDelve = 52,
    PanCamera = 53,
    Follow = 54,
    Stick = 55,
    Face = 56,
    Craft = 57,
    Sprint = 58,
    CommandWindow = 59,
    RightHandWeapon = 60,
    TwoHandedWeapon = 61,
    RangedWeapon = 62,
    UseItemSecondary = 63,
    NearestLoot = 64,
    MousePan = 65,
    MapWindow = 66,
    ToggleInterface = 67,
    RealmWarMap = 68,
    QuestJournal = 69,
    RunLock2 = 70,
    ShowCharacterInfo = 71,
    TakeScreenshot = 72,
    CameraToggle = 73,
}

impl GameAction {
    /// Compat aliases used by older call sites / docs. Same discriminants as the table names.
    #[allow(non_upper_case_globals)]
    pub const MoveForward: Self = Self::Forward;
    #[allow(non_upper_case_globals)]
    pub const MoveBack: Self = Self::Back;
    #[allow(non_upper_case_globals)]
    pub const Jump: Self = Self::JumpUp;
    #[allow(non_upper_case_globals)]
    pub const StrafeLeft: Self = Self::SlideLeft;
    #[allow(non_upper_case_globals)]
    pub const StrafeRight: Self = Self::SlideRight;
    #[allow(non_upper_case_globals)]
    pub const Attack: Self = Self::CombatMode;
    #[allow(non_upper_case_globals)]
    pub const TargetNearest: Self = Self::TargetEnemy;
    #[allow(non_upper_case_globals)]
    pub const OpenChat: Self = Self::Chat;
    #[allow(non_upper_case_globals)]
    pub const ToggleCameraSnapback: Self = Self::CameraToggle;

    /// Table index 0..73.
    #[must_use]
    pub const fn index(self) -> u8 {
        self as u8
    }

    /// Inverse of [`Self::index`].
    #[must_use]
    pub fn from_index(i: u8) -> Option<Self> {
        ACTIONS_TABLE.get(i as usize).map(|e| e.action)
    }

    /// The config-file name for this action. Stable — changing one silently orphans a user's
    /// existing binding, so treat these as the file format they are.
    #[must_use]
    pub fn name(self) -> String {
        entry_for(self)
            .expect("every GameAction is in ACTIONS_TABLE")
            .config_name
            .into()
    }

    /// Parse a config-file action name (table `config_name`, plus [`meta::LEGACY_ALIASES`]).
    #[must_use]
    pub fn from_name(s: &str) -> Option<Self> {
        let canonical = meta::canonical_action_token(s);
        ACTIONS_TABLE
            .iter()
            .find(|e| e.config_name == canonical)
            .map(|e| e.action)
    }

    /// Every action in table-index order. Length is always [`TABLE_ACTION_COUNT`] (74).
    #[must_use]
    pub fn all() -> Vec<Self> {
        ACTIONS_TABLE.iter().map(|e| e.action).collect()
    }

    /// Whether this action is a HELD state (movement / look) rather than a one-shot press.
    ///
    /// Held actions must also fire on key-release so the state clears; edge actions must fire only
    /// on press, or a single tap triggers them twice.
    #[must_use]
    pub fn is_held(self) -> bool {
        matches!(
            self,
            Self::Forward
                | Self::Back
                | Self::SlideLeft
                | Self::SlideRight
                | Self::LookUp
                | Self::LookDown
                | Self::Forward2
                | Self::Back2
                | Self::PanCamera
                | Self::MousePan
                | Self::Mouse
                | Self::Sprint
                | Self::Walk
        )
    }
}

/// The config-file name for a physical key, and back.
///
/// Only the keys worth binding are covered; an unknown name is reported to the player rather than
/// silently dropped, because a typo'd binding that vanishes is far more confusing than one that
/// complains.
#[must_use]
pub fn key_name(code: KeyCode) -> Option<&'static str> {
    Some(match code {
        KeyCode::KeyA => "A",
        KeyCode::KeyB => "B",
        KeyCode::KeyC => "C",
        KeyCode::KeyD => "D",
        KeyCode::KeyE => "E",
        KeyCode::KeyF => "F",
        KeyCode::KeyG => "G",
        KeyCode::KeyH => "H",
        KeyCode::KeyI => "I",
        KeyCode::KeyJ => "J",
        KeyCode::KeyK => "K",
        KeyCode::KeyL => "L",
        KeyCode::KeyM => "M",
        KeyCode::KeyN => "N",
        KeyCode::KeyO => "O",
        KeyCode::KeyP => "P",
        KeyCode::KeyQ => "Q",
        KeyCode::KeyR => "R",
        KeyCode::KeyS => "S",
        KeyCode::KeyT => "T",
        KeyCode::KeyU => "U",
        KeyCode::KeyV => "V",
        KeyCode::KeyW => "W",
        KeyCode::KeyX => "X",
        KeyCode::KeyY => "Y",
        KeyCode::KeyZ => "Z",
        KeyCode::Digit0 => "0",
        KeyCode::Digit1 => "1",
        KeyCode::Digit2 => "2",
        KeyCode::Digit3 => "3",
        KeyCode::Digit4 => "4",
        KeyCode::Digit5 => "5",
        KeyCode::Digit6 => "6",
        KeyCode::Digit7 => "7",
        KeyCode::Digit8 => "8",
        KeyCode::Digit9 => "9",
        KeyCode::ArrowUp => "Up",
        KeyCode::ArrowDown => "Down",
        KeyCode::ArrowLeft => "Left",
        KeyCode::ArrowRight => "Right",
        KeyCode::Space => "Space",
        KeyCode::Tab => "Tab",
        KeyCode::Enter => "Enter",
        KeyCode::NumpadEnter => "NumpadEnter",
        KeyCode::Escape => "Escape",
        KeyCode::F1 => "F1",
        KeyCode::F2 => "F2",
        KeyCode::F3 => "F3",
        KeyCode::F4 => "F4",
        KeyCode::F5 => "F5",
        KeyCode::F6 => "F6",
        KeyCode::F7 => "F7",
        KeyCode::F8 => "F8",
        KeyCode::F9 => "F9",
        KeyCode::F10 => "F10",
        KeyCode::F11 => "F11",
        KeyCode::F12 => "F12",
        KeyCode::PageUp => "PageUp",
        KeyCode::PageDown => "PageDown",
        KeyCode::Home => "Home",
        KeyCode::End => "End",
        KeyCode::Insert => "Insert",
        KeyCode::Delete => "Delete",
        KeyCode::CapsLock => "CapsLock",
        KeyCode::Comma => "Comma",
        KeyCode::Period => "Period",
        KeyCode::Slash => "Slash",
        KeyCode::Semicolon => "Semicolon",
        KeyCode::Quote => "Quote",
        KeyCode::BracketLeft => "BracketLeft",
        KeyCode::BracketRight => "BracketRight",
        KeyCode::Backslash => "Backslash",
        KeyCode::Backquote => "Backquote",
        KeyCode::Minus => "Minus",
        KeyCode::Equal => "Equal",
        KeyCode::Numpad0 => "Numpad0",
        KeyCode::Numpad1 => "Numpad1",
        KeyCode::Numpad2 => "Numpad2",
        KeyCode::Numpad3 => "Numpad3",
        KeyCode::Numpad4 => "Numpad4",
        KeyCode::Numpad5 => "Numpad5",
        KeyCode::Numpad6 => "Numpad6",
        KeyCode::Numpad7 => "Numpad7",
        KeyCode::Numpad8 => "Numpad8",
        KeyCode::Numpad9 => "Numpad9",
        KeyCode::NumpadAdd => "NumpadAdd",
        KeyCode::NumpadSubtract => "NumpadSubtract",
        KeyCode::NumpadMultiply => "NumpadMultiply",
        KeyCode::NumpadDivide => "NumpadDivide",
        _ => return None,
    })
}

/// Render a chord as config text: `Alt+Z`, `Ctrl+Shift+F1`, or a bare key name.
///
/// Modifiers are part of the saved form because they are part of the binding. Writing only the key
/// meant a saved Alt+Z reloaded as a bare Z — silently rebinding the interface toggle onto a letter
/// the player types constantly.
#[must_use]
pub fn chord_name(c: Chord) -> Option<String> {
    let k = key_name(c.key)?;
    let mut out = String::new();
    if c.ctrl {
        out.push_str("Ctrl+");
    }
    if c.alt {
        out.push_str("Alt+");
    }
    if c.shift {
        out.push_str("Shift+");
    }
    out.push_str(k);
    Some(out)
}

/// Parse `Alt+Z` / `ctrl+shift+f1` / `W` into a chord. Modifier order does not matter.
#[must_use]
pub fn chord_from_name(s: &str) -> Option<Chord> {
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut rest = s.trim();
    while let Some((head, tail)) = rest.split_once('+') {
        match head.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" => alt = true,
            "shift" => shift = true,
            // Not a modifier — the '+' belongs to the key name itself, so stop splitting.
            _ => break,
        }
        rest = tail.trim();
    }
    Some(Chord {
        key: key_from_name(rest)?,
        ctrl,
        alt,
        shift,
    })
}

pub fn key_from_name(s: &str) -> Option<KeyCode> {
    let s = s.trim();
    // Derived from `key_name` rather than a second hand-written table: two tables would drift, and
    // a key that serialises to a name it cannot parse back is an unloadable config.
    ALL_BINDABLE
        .iter()
        .copied()
        .find(|&k| key_name(k).is_some_and(|n| n.eq_ignore_ascii_case(s)))
}

/// Every key this system can bind. Also the round-trip domain for the name tests.
pub const ALL_BINDABLE: &[KeyCode] = &[
    KeyCode::KeyA,
    KeyCode::KeyB,
    KeyCode::KeyC,
    KeyCode::KeyD,
    KeyCode::KeyE,
    KeyCode::KeyF,
    KeyCode::KeyG,
    KeyCode::KeyH,
    KeyCode::KeyI,
    KeyCode::KeyJ,
    KeyCode::KeyK,
    KeyCode::KeyL,
    KeyCode::KeyM,
    KeyCode::KeyN,
    KeyCode::KeyO,
    KeyCode::KeyP,
    KeyCode::KeyQ,
    KeyCode::KeyR,
    KeyCode::KeyS,
    KeyCode::KeyT,
    KeyCode::KeyU,
    KeyCode::KeyV,
    KeyCode::KeyW,
    KeyCode::KeyX,
    KeyCode::KeyY,
    KeyCode::KeyZ,
    KeyCode::Digit0,
    KeyCode::Digit1,
    KeyCode::Digit2,
    KeyCode::Digit3,
    KeyCode::Digit4,
    KeyCode::Digit5,
    KeyCode::Digit6,
    KeyCode::Digit7,
    KeyCode::Digit8,
    KeyCode::Digit9,
    KeyCode::ArrowUp,
    KeyCode::ArrowDown,
    KeyCode::ArrowLeft,
    KeyCode::ArrowRight,
    KeyCode::Space,
    KeyCode::Tab,
    KeyCode::Enter,
    KeyCode::NumpadEnter,
    KeyCode::Escape,
    KeyCode::F1,
    KeyCode::F2,
    KeyCode::F3,
    KeyCode::F4,
    KeyCode::F5,
    KeyCode::F6,
    KeyCode::F7,
    KeyCode::F8,
    KeyCode::F9,
    KeyCode::F10,
    KeyCode::F11,
    KeyCode::F12,
    KeyCode::PageUp,
    KeyCode::PageDown,
    KeyCode::Home,
    KeyCode::End,
    KeyCode::Insert,
    KeyCode::Delete,
    KeyCode::CapsLock,
    KeyCode::Comma,
    KeyCode::Period,
    KeyCode::Slash,
    KeyCode::Semicolon,
    KeyCode::Quote,
    KeyCode::BracketLeft,
    KeyCode::BracketRight,
    KeyCode::Backslash,
    KeyCode::Backquote,
    KeyCode::Minus,
    KeyCode::Equal,
    KeyCode::Numpad0,
    KeyCode::Numpad1,
    KeyCode::Numpad2,
    KeyCode::Numpad3,
    KeyCode::Numpad4,
    KeyCode::Numpad5,
    KeyCode::Numpad6,
    KeyCode::Numpad7,
    KeyCode::Numpad8,
    KeyCode::Numpad9,
    KeyCode::NumpadAdd,
    KeyCode::NumpadSubtract,
    KeyCode::NumpadMultiply,
    KeyCode::NumpadDivide,
];

/// A physical key plus the modifiers held with it.
///
/// The binding map was keyed on a bare `KeyCode`, so a modified chord could not even be expressed —
/// which is why the interface toggle sat on a lone F1 instead of the client's Alt+Z. DAoC's own
/// `keyboard.dat` carries a `shiftNN` value beside every `keyNN`, so modifiers are part of a
/// binding in the original too; this is the shape the real data already has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub key: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Chord {
    /// An unmodified key.
    #[must_use]
    pub const fn plain(key: KeyCode) -> Self {
        Self {
            key,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }
    /// `Alt` + key.
    #[must_use]
    pub const fn alt(key: KeyCode) -> Self {
        Self {
            key,
            ctrl: false,
            alt: true,
            shift: false,
        }
    }
    /// `Ctrl` + key.
    #[must_use]
    pub const fn ctrl(key: KeyCode) -> Self {
        Self {
            key,
            ctrl: true,
            alt: false,
            shift: false,
        }
    }
    /// `Shift` + key.
    #[must_use]
    pub const fn shift(key: KeyCode) -> Self {
        Self {
            key,
            ctrl: false,
            alt: false,
            shift: true,
        }
    }
}

/// The player's key → action map.
///
/// Keyed by CHORD, not by action, because that is the direction the input handler asks in — and it
/// makes the "one chord does one thing" rule structural rather than something to remember to
/// enforce. An action may still have several chords (W and Up both walk forward).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bindings {
    by_key: HashMap<Chord, GameAction>,
}

impl Default for Bindings {
    fn default() -> Self {
        Self::daoc_defaults()
    }
}

impl Bindings {
    /// Defaults for all 74 table actions: each action's **primary** chord from [`ACTIONS_TABLE`],
    /// plus secondary aliases for forward/back on the arrow keys (DAoC muscle memory).
    ///
    /// Turn left/right are **not** in the 74-name internal table (they appear only in the 36-name
    /// display table); A/D are therefore left free here until that mapping is pinned.
    #[must_use]
    pub fn daoc_defaults() -> Self {
        let mut by_key: HashMap<Chord, GameAction> = HashMap::with_capacity(TABLE_ACTION_COUNT + 2);
        for e in &ACTIONS_TABLE {
            by_key.insert(e.primary, e.action);
        }
        // Secondary movement aliases — not primaries; must not collide with any primary chord.
        by_key.insert(Chord::plain(KeyCode::ArrowUp), GameAction::Forward);
        by_key.insert(Chord::plain(KeyCode::ArrowDown), GameAction::Back);
        Self { by_key }
    }

    /// What this key does, if anything.
    #[must_use]
    pub fn action_for(&self, key: KeyCode) -> Option<GameAction> {
        self.action_for_chord(Chord::plain(key))
    }

    /// What this chord does, if anything.
    #[must_use]
    pub fn action_for_chord(&self, chord: Chord) -> Option<GameAction> {
        self.by_key.get(&chord).copied()
    }

    /// Every key bound to an action, in a stable order so `/keyboard` output doesn't shuffle
    /// between runs (HashMap iteration order is not deterministic).
    #[must_use]
    pub fn keys_for(&self, action: GameAction) -> Vec<KeyCode> {
        self.chords_for(action).into_iter().map(|c| c.key).collect()
    }

    /// Every CHORD bound to an action, in a stable order.
    #[must_use]
    pub fn chords_for(&self, action: GameAction) -> Vec<Chord> {
        let mut v: Vec<Chord> = self
            .by_key
            .iter()
            .filter(|(_, &a)| a == action)
            .map(|(&k, _)| k)
            .collect();
        v.sort_by_key(|c| chord_name(*c).unwrap_or_default());
        v
    }

    /// The primary default chord for this action from the table (ignores runtime rebinds).
    #[must_use]
    pub fn table_primary(action: GameAction) -> Option<Chord> {
        entry_for(action).map(|e| e.primary)
    }

    /// Bind a key to an action, releasing whatever else that key did.
    ///
    /// Rebinding does NOT clear the action's other keys: binding `Up` to forward should not unbind
    /// `W`. Clearing an action is `unbind_action`, which is the explicit request.
    pub fn bind(&mut self, key: KeyCode, action: GameAction) {
        self.bind_chord(Chord::plain(key), action);
    }

    /// Bind a modified chord to an action.
    pub fn bind_chord(&mut self, chord: Chord, action: GameAction) {
        self.by_key.insert(chord, action);
    }

    /// Remove every key bound to an action. Returns how many were cleared.
    pub fn unbind_action(&mut self, action: GameAction) -> usize {
        let before = self.by_key.len();
        self.by_key.retain(|_, &mut a| a != action);
        before - self.by_key.len()
    }

    /// Actions with no key at all — what `/keyboard` should warn about, since an unbound movement
    /// key leaves the player unable to move with no on-screen explanation.
    #[must_use]
    pub fn unbound(&self) -> Vec<GameAction> {
        GameAction::all()
            .into_iter()
            .filter(|&a| self.keys_for(a).is_empty())
            .collect()
    }

    /// Serialise to the config format: `action = KEY` lines, one per bound key.
    #[must_use]
    pub fn to_config(&self) -> String {
        let mut out = String::from(
            "# CAER keybindings. One line per bound key; an action may appear more than once.\n\
             # Key names: letters A-Z, digits 0-9, arrows, Space, Tab, Enter, Escape, F1-F12,\n\
             # PageUp/Down, Home/End, Insert/Delete, punctuation, numpad. Modifiers: `Alt+Z`.\n\
             # 74 actions from game1127.dll keyboard_actions_internal.tsv.\n\
             # Delete this file to return to the table defaults (W/S move, Q/E slide).\n\n",
        );
        for action in GameAction::all() {
            for chord in self.chords_for(action) {
                if let Some(k) = chord_name(chord) {
                    out.push_str(&format!("{} = {}\n", action.name(), k));
                }
            }
        }
        out
    }

    /// Parse the config format, returning the bindings plus any complaints.
    ///
    /// A malformed line is reported and skipped rather than failing the whole load: one typo should
    /// not drop the player into a client with no controls at all. Parsing starts from the DEFAULTS
    /// so a partial config (the common hand-edited case — someone rebinding one key) keeps
    /// everything they did not mention.
    #[must_use]
    pub fn parse(text: &str) -> (Self, Vec<String>) {
        let mut binds = Self::daoc_defaults();
        let mut warnings = Vec::new();
        // Any action the file mentions is taken over WHOLESALE: if the config binds forward to
        // Up, the default W must go, or the file could never remove a binding.
        let mut cleared: Vec<GameAction> = Vec::new();

        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((lhs, rhs)) = line.split_once('=') else {
                warnings.push(format!(
                    "line {}: expected `action = KEY`, got `{}`",
                    n + 1,
                    raw.trim()
                ));
                continue;
            };
            let Some(action) = GameAction::from_name(lhs) else {
                warnings.push(format!("line {}: unknown action `{}`", n + 1, lhs.trim()));
                continue;
            };
            let Some(key) = chord_from_name(rhs) else {
                warnings.push(format!("line {}: unknown key `{}`", n + 1, rhs.trim()));
                continue;
            };
            if !cleared.contains(&action) {
                binds.unbind_action(action);
                cleared.push(action);
            }
            binds.bind_chord(key, action);
        }
        (binds, warnings)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::*;

    /// Every bindable key must survive name → key → name. A key that serialises to a name it cannot
    /// parse back produces a config file the client refuses to load.
    #[test]
    fn key_names_round_trip() {
        for &k in ALL_BINDABLE {
            let name = key_name(k).unwrap_or_else(|| panic!("{k:?} is bindable but has no name"));
            assert_eq!(
                key_from_name(name),
                Some(k),
                "{name} did not parse back to {k:?}"
            );
            assert_eq!(key_from_name(&name.to_ascii_lowercase()), Some(k));
        }
    }

    /// Same contract for action names, including legacy aliases from the pre-74 set.
    #[test]
    fn action_names_round_trip() {
        for a in GameAction::all() {
            assert_eq!(
                GameAction::from_name(&a.name()),
                Some(a),
                "{a:?} did not round-trip"
            );
        }
        assert_eq!(
            GameAction::from_name("move_forward"),
            Some(GameAction::Forward)
        );
        assert_eq!(
            GameAction::from_name("strafe_left"),
            Some(GameAction::SlideLeft)
        );
        assert_eq!(
            GameAction::from_name("attack"),
            Some(GameAction::CombatMode)
        );
        assert_eq!(GameAction::from_name("open_chat"), Some(GameAction::Chat));
    }

    /// Falsifier: the embedded table is exactly the 74-row DLL extract — drop a row and this fails.
    #[test]
    fn table_has_exactly_seventy_four_actions() {
        assert_eq!(
            TABLE_ACTION_COUNT, 74,
            "leg 15 requires the 74-action internal table"
        );
        assert_eq!(ACTIONS_TABLE.len(), 74);
        assert_eq!(GameAction::all().len(), 74);
    }

    /// Falsifier over the **whole** table (not a sample):
    ///
    /// **Binding** here means the table's **primary** [`Chord`] — the single default identity for
    /// each action (DAoC `keyboard.dat` stores one key+modifier per slot). Secondary aliases (arrow
    /// keys for forward/back) are allowed in [`Bindings::daoc_defaults`] but are not primaries.
    ///
    /// Properties checked for every row 0..73:
    /// 1. contiguous unique indices;
    /// 2. `GameAction` discriminant matches index (bijection);
    /// 3. exactly one primary chord per action;
    /// 4. no two actions share a primary chord;
    /// 5. defaults expose that primary (lookup succeeds);
    /// 6. no chord in the live map points at two actions (HashMap invariant, re-checked).
    #[test]
    fn every_table_action_has_exactly_one_unique_primary_binding() {
        let mut seen_indices = HashSet::new();
        let mut seen_actions = HashSet::new();
        let mut primary_owner: HashMap<Chord, GameAction> = HashMap::new();

        for (i, e) in ACTIONS_TABLE.iter().enumerate() {
            assert_eq!(e.index as usize, i, "table must be in index order");
            assert!(
                seen_indices.insert(e.index),
                "duplicate table index {}",
                e.index
            );
            assert!(
                seen_actions.insert(e.action),
                "duplicate GameAction {:?} at index {}",
                e.action,
                e.index
            );
            assert_eq!(
                e.action.index(),
                e.index,
                "{:?} discriminant must equal table index",
                e.action
            );
            assert_eq!(
                GameAction::from_index(e.index),
                Some(e.action),
                "from_index({})",
                e.index
            );
            assert_eq!(e.config_name, e.action.name());
            assert!(
                !e.internal_name.is_empty(),
                "index {} missing DLL internal name",
                e.index
            );

            if let Some(prev) = primary_owner.insert(e.primary, e.action) {
                panic!(
                    "primary chord {:?} shared by {:?} and {:?} — two actions must not share one primary binding",
                    e.primary, prev, e.action
                );
            }
        }

        assert_eq!(seen_indices.len(), 74);
        assert_eq!(seen_actions.len(), 74);
        assert_eq!(primary_owner.len(), 74);

        let binds = Bindings::daoc_defaults();
        for e in &ACTIONS_TABLE {
            assert_eq!(
                binds.action_for_chord(e.primary),
                Some(e.action),
                "defaults must dispatch primary for {:?} ({})",
                e.action,
                e.internal_name
            );
            let chords = binds.chords_for(e.action);
            assert!(!chords.is_empty(), "{:?} unbound in defaults", e.action);
            assert!(
                chords.contains(&e.primary),
                "{:?} defaults missing its table primary {:?}",
                e.action,
                e.primary
            );
            // Exactly one primary identity in the table sense: the table lists one; defaults may
            // add secondaries, but the primary row is unique and present.
            let primaries_for_action: Vec<_> = ACTIONS_TABLE
                .iter()
                .filter(|row| row.action == e.action)
                .collect();
            assert_eq!(
                primaries_for_action.len(),
                1,
                "{:?} must appear exactly once in ACTIONS_TABLE",
                e.action
            );
        }

        assert!(
            binds.unbound().is_empty(),
            "unbound by default: {:?}",
            binds.unbound()
        );

        // Structural: each live chord maps to one action (no silent multi-dispatch).
        let mut chord_to_action: HashMap<Chord, GameAction> = HashMap::new();
        for a in GameAction::all() {
            for c in binds.chords_for(a) {
                if let Some(prev) = chord_to_action.insert(c, a) {
                    panic!("chord {:?} bound to both {:?} and {:?}", c, prev, a);
                }
            }
        }
    }

    /// Dropping an action from the table must be visible: index set would no longer be 0..73.
    #[test]
    fn dropping_an_action_breaks_index_contiguity_check() {
        let indices: Vec<u8> = ACTIONS_TABLE.iter().map(|e| e.index).collect();
        assert_eq!(indices, (0u8..74).collect::<Vec<_>>());
    }

    /// Movement / look held-state contract for the table actions that need key-up clearing.
    #[test]
    fn held_actions_cover_movement_and_look() {
        for a in [
            GameAction::Forward,
            GameAction::Back,
            GameAction::SlideLeft,
            GameAction::SlideRight,
            GameAction::LookUp,
            GameAction::LookDown,
        ] {
            assert!(a.is_held(), "{a:?} is a held state");
        }
        for a in [
            GameAction::JumpUp,
            GameAction::CombatMode,
            GameAction::TargetEnemy,
            GameAction::Chat,
            GameAction::ToggleInterface,
            GameAction::Sit,
        ] {
            assert!(!a.is_held(), "{a:?} is a one-shot");
        }
    }

    /// Core DAoC muscle-memory chords still resolve after the table expansion.
    #[test]
    fn defaults_keep_core_daoc_movement_and_combat() {
        let b = Bindings::default();
        assert_eq!(b.action_for(KeyCode::KeyW), Some(GameAction::Forward));
        assert_eq!(b.action_for(KeyCode::KeyS), Some(GameAction::Back));
        assert_eq!(b.action_for(KeyCode::KeyQ), Some(GameAction::SlideLeft));
        assert_eq!(b.action_for(KeyCode::KeyE), Some(GameAction::SlideRight));
        assert_eq!(b.action_for(KeyCode::Space), Some(GameAction::JumpUp));
        assert_eq!(b.action_for(KeyCode::KeyF), Some(GameAction::CombatMode));
        assert_eq!(b.action_for(KeyCode::Tab), Some(GameAction::TargetEnemy));
        assert_eq!(b.action_for(KeyCode::Enter), Some(GameAction::Chat));
        assert_eq!(
            b.action_for_chord(Chord::alt(KeyCode::KeyZ)),
            Some(GameAction::ToggleInterface)
        );
        assert_eq!(b.action_for(KeyCode::F3), Some(GameAction::CameraToggle));
        // A/D are free: Turn Left/Right are not in the 74-name internal table.
        assert_eq!(b.action_for(KeyCode::KeyA), None);
        assert_eq!(b.action_for(KeyCode::KeyD), None);
    }

    /// A whole config must survive a save/load cycle unchanged.
    #[test]
    fn config_round_trips_through_text() {
        let b = Bindings::default();
        let (back, warnings) = Bindings::parse(&b.to_config());
        assert!(warnings.is_empty(), "clean config warned: {warnings:?}");
        assert_eq!(back, b, "a saved config did not reload identically");
    }

    /// A config that mentions an action REPLACES its keys — otherwise a rebind could only ever add,
    /// and the default W would haunt a player who moved forward to Up.
    #[test]
    fn a_rebound_action_loses_its_default_keys() {
        let (b, warnings) = Bindings::parse("forward = Up");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(b.action_for(KeyCode::ArrowUp), Some(GameAction::Forward));
        assert_eq!(
            b.action_for(KeyCode::KeyW),
            None,
            "W must be released by the rebind"
        );
        assert_eq!(b.action_for(KeyCode::KeyQ), Some(GameAction::SlideLeft));
    }

    /// Two lines for the same action bind BOTH keys — that is how W and Up coexist.
    #[test]
    fn an_action_can_hold_several_keys() {
        let (b, _) = Bindings::parse("forward = W\nforward = Up");
        assert_eq!(b.action_for(KeyCode::KeyW), Some(GameAction::Forward));
        assert_eq!(b.action_for(KeyCode::ArrowUp), Some(GameAction::Forward));
        assert_eq!(b.keys_for(GameAction::Forward).len(), 2);
    }

    /// One key cannot do two things: binding it again releases the old action.
    #[test]
    fn binding_a_key_twice_keeps_only_the_last() {
        let mut b = Bindings::default();
        b.bind(KeyCode::KeyW, GameAction::CombatMode);
        assert_eq!(b.action_for(KeyCode::KeyW), Some(GameAction::CombatMode));
        assert!(!b.keys_for(GameAction::Forward).contains(&KeyCode::KeyW));
        assert_eq!(b.keys_for(GameAction::Forward), vec![KeyCode::ArrowUp]);
    }

    /// A typo must be REPORTED and skipped, never silently dropped and never fatal.
    #[test]
    fn bad_lines_warn_without_losing_the_rest() {
        let (b, warnings) = Bindings::parse(
            "# a comment\n\
             slide_left = Q\n\
             fly_to_the_moon = X\n\
             combat_mode = NoSuchKey\n\
             this line has no equals\n\
             combat_mode = R\n",
        );
        assert_eq!(warnings.len(), 3, "expected 3 complaints, got {warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("unknown action")));
        assert!(warnings.iter().any(|w| w.contains("unknown key")));
        assert!(warnings
            .iter()
            .any(|w| w.contains("expected `action = KEY`")));
        assert_eq!(b.action_for(KeyCode::KeyQ), Some(GameAction::SlideLeft));
        assert_eq!(b.action_for(KeyCode::KeyR), Some(GameAction::CombatMode));
        assert!(!b.keys_for(GameAction::CombatMode).is_empty());
    }

    /// Comments and blank lines are ignored, and trailing comments don't end up in key names.
    #[test]
    fn comments_and_whitespace_are_ignored() {
        let (b, warnings) =
            Bindings::parse("\n  # header\n\n  combat_mode  =  R   # inline note\n");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(b.action_for(KeyCode::KeyR), Some(GameAction::CombatMode));
    }

    /// The interface toggle is the client's Alt+Z, and a bare Z must NOT fire it.
    #[test]
    fn interface_toggle_is_alt_z_and_not_bare_z() {
        let b = Bindings::daoc_defaults();
        assert_eq!(
            b.action_for_chord(Chord::alt(KeyCode::KeyZ)),
            Some(GameAction::ToggleInterface),
            "Alt+Z should toggle the interface"
        );
        assert_eq!(
            b.action_for(KeyCode::KeyZ),
            Some(GameAction::LastAttacker),
            "bare Z is Last Attacker in the 74-table defaults"
        );
        assert_eq!(
            b.action_for(KeyCode::F1),
            Some(GameAction::TargetGroup1),
            "F1 is Target Group1 in DAoC, not a UI toggle"
        );
    }

    /// A plain binding must not fire when modifiers are held: Alt+W is not Forward.
    #[test]
    fn modifiers_change_the_binding() {
        let b = Bindings::daoc_defaults();
        assert_eq!(b.action_for(KeyCode::KeyW), Some(GameAction::Forward));
        assert_eq!(b.action_for_chord(Chord::alt(KeyCode::KeyW)), None);
    }

    /// Compat associated-const aliases still pattern-match / compare as the table actions.
    #[test]
    fn legacy_aliases_point_at_table_actions() {
        assert_eq!(GameAction::MoveForward, GameAction::Forward);
        assert_eq!(GameAction::StrafeLeft, GameAction::SlideLeft);
        assert_eq!(GameAction::Attack, GameAction::CombatMode);
        assert_eq!(GameAction::OpenChat, GameAction::Chat);
        assert_eq!(GameAction::ToggleCameraSnapback, GameAction::CameraToggle);
    }
}
