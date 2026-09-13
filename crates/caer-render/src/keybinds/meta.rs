//! Canonical binding metadata — one source for runtime names, aliases, and player-facing help.
//!
//! `ACTIONS_TABLE` owns the 74 DLL-extracted actions and their primary default chords.
//! This module owns:
//! - legacy config/CLI aliases (`attack` → `combat_mode`, …);
//! - which actions appear in compact player help, and their short labels;
//! - help/header text generated from a live [`Bindings`] map (defaults or rebound).
//!
//! Falsifier: changing a table primary or rebinding at runtime must change the generated help;
//! listing `/keyboard` and compact help must never invent chords the bindings map does not hold.

use super::{chord_name, entry_for, ActionTableEntry, Bindings, GameAction, ACTIONS_TABLE};

/// Legacy config / `/keyboard` names → canonical [`ActionTableEntry::config_name`].
///
/// Input accepts these; reports and help always print the canonical name.
pub const LEGACY_ALIASES: &[(&str, &str)] = &[
    ("move_forward", "forward"),
    ("move_back", "back"),
    ("jump", "jump_up"),
    ("strafe_left", "slide_left"),
    ("strafe_right", "slide_right"),
    ("attack", "combat_mode"),
    ("target_nearest", "target_enemy"),
    ("open_chat", "chat"),
    ("toggle_camera_snapback", "camera_toggle"),
];

/// Resolve a player/config token to a canonical config name (alias → table name, or passthrough).
#[must_use]
pub fn canonical_action_token(raw: &str) -> String {
    let s = raw.trim().to_ascii_lowercase();
    for &(alias, canon) in LEGACY_ALIASES {
        if s == alias {
            return canon.to_string();
        }
    }
    s
}

/// Legacy aliases that resolve to this action's canonical config name.
#[must_use]
pub fn aliases_for(action: GameAction) -> Vec<&'static str> {
    let Some(name) = entry_for(action).map(|e| e.config_name) else {
        return Vec::new();
    };
    LEGACY_ALIASES
        .iter()
        .filter(|(_, canon)| *canon == name)
        .map(|(alias, _)| *alias)
        .collect()
}

/// Compact player-help rows: action + short verb shown next to the live chord(s).
///
/// Order is the product control strip (movement → combat → targeting → chat → UI), not table index.
/// Keyboard turn (display-table "Turn Left/Right") is **not** here: those names are absent from the
/// 74-action internal table, A/D are unbound by default, and mouse drag steers facing.
/// Actions the product `rustdaoc` dispatcher maps to a [`crate::product_input::ProductInput`].
///
/// **Not** a stock-parity claim. Advertising uses [`super::parity::action_parity`]; only
/// [`super::parity::ActionParity::Proven`] may appear as implemented in player help.
pub const PRODUCT_WIRED: &[GameAction] = &[
    GameAction::Forward,
    GameAction::Back,
    GameAction::SlideLeft,
    GameAction::SlideRight,
    GameAction::JumpUp,
    GameAction::Walk,
    GameAction::Sprint,
    GameAction::Forward2,
    GameAction::Back2,
    GameAction::Chat,
    GameAction::Destroy,
    GameAction::TargetEnemy,
    GameAction::TargetFriend,
    GameAction::TargetObject,
    GameAction::LastAttacker,
    GameAction::CombatMode,
    GameAction::Sit,
    GameAction::ToggleInterface,
    GameAction::TakeScreenshot,
    GameAction::CameraToggle,
    GameAction::Inventory,
    GameAction::Stats,
    GameAction::ShowCharacterInfo,
    GameAction::Group,
    GameAction::MapWindow,
    GameAction::Compass,
    GameAction::QuestJournal,
    GameAction::ShowSkills,
    GameAction::ShowSpells,
    GameAction::ShowCombat,
    GameAction::Open,
    GameAction::Get,
    GameAction::NearestLoot,
    GameAction::Follow,
    GameAction::Stick,
    GameAction::Face,
    GameAction::Reply,
    GameAction::Consider,
    GameAction::UseItem,
    GameAction::UseItemSecondary,
    GameAction::Sell,
    GameAction::Runlock,
    GameAction::RunLock2,
    GameAction::GroundTarget,
    GameAction::TargetGroup1,
    GameAction::TargetGroup2,
    GameAction::TargetGroup3,
    GameAction::TargetGroup4,
    GameAction::TargetGroup5,
    GameAction::TargetGroup6,
    GameAction::TargetGroup7,
    GameAction::TargetGroup8,
    GameAction::Craft,
    GameAction::CommandWindow,
    GameAction::RightHandWeapon,
    GameAction::TwoHandedWeapon,
    GameAction::RangedWeapon,
    GameAction::RealmWarMap,
    GameAction::LookUp,
    GameAction::LookDown,
    GameAction::ResetCamera,
    GameAction::ToggleNames,
    GameAction::MouseLookToggle,
    GameAction::Torch,
    GameAction::PerfMeter,
    GameAction::InformationDelve,
    GameAction::ChatLog,
    GameAction::PageUp,
    GameAction::PageDown,
    GameAction::PageUpSys,
    GameAction::PageDownSys,
    GameAction::PanCamera,
    GameAction::MousePan,
    GameAction::Mouse,
];

#[must_use]
pub fn is_product_wired(action: GameAction) -> bool {
    PRODUCT_WIRED.contains(&action)
}

pub const COMPACT_HELP: &[(GameAction, &str)] = &[
    (GameAction::Forward, "run forward"),
    (GameAction::Back, "run back"),
    (GameAction::SlideLeft, "strafe left"),
    (GameAction::SlideRight, "strafe right"),
    (GameAction::JumpUp, "jump"),
    (GameAction::CombatMode, "combat mode"),
    (GameAction::CameraToggle, "camera snapback"),
    (GameAction::ResetCamera, "reset camera"),
    (GameAction::MouseLookToggle, "mouse look"),
];

/// Format the chords currently bound to `action`, or `(unbound)`.
#[must_use]
pub fn format_bound_chords(binds: &Bindings, action: GameAction) -> String {
    let chords = binds.chords_for(action);
    if chords.is_empty() {
        return "(unbound)".to_string();
    }
    chords
        .into_iter()
        .filter_map(chord_name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// One line per compact-help action, reflecting the live binding map (defaults or rebound).
/// Partial/Unsupported rows must carry [`super::parity::ActionParity::help_suffix`] — never
/// silently look Proven.
#[must_use]
pub fn compact_controls_lines(binds: &Bindings) -> Vec<String> {
    let mut lines = Vec::with_capacity(COMPACT_HELP.len() + 4);
    lines.push(
        "Controls (from live bindings — `/keyboard` lists all 74; unimplemented actions are labeled):"
            .into(),
    );
    for &(action, label) in COMPACT_HELP {
        let chords = format_bound_chords(binds, action);
        let name = action.name();
        let suffix = super::parity::action_parity(action).help_suffix();
        lines.push(format!("  {chords:<16} {name} — {label}{suffix}"));
    }
    lines.push("  L-drag           orbit camera (character facing unchanged)".into());
    lines.push("  R-drag           turn character; camera snaps behind unless snapback off".into());
    lines.push("  1-0              quickbar slots (plain digits; not GameAction rows)".into());
    lines.push(
        "  A/D              unbound — display-table Turn Left/Right not in 74-action internal table; mouse steers facing"
            .into(),
    );
    lines
}

/// Full player-facing controls block for `--help` / operator docs.
#[must_use]
pub fn player_controls_help(binds: &Bindings) -> String {
    let mut out = compact_controls_lines(binds).join("\n");
    out.push('\n');
    out.push_str(
        "Canonical action names match keybinds.cfg / `/keyboard` (e.g. combat_mode).\n\
         Legacy aliases still resolve on input (attack → combat_mode) but are never printed.\n\
         Movement is character-relative (W along facing); Q/E are slide_left/slide_right.\n",
    );
    out
}

/// `/keyboard` listing lines derived from the same metadata as runtime dispatch.
#[must_use]
pub fn keyboard_list_lines(binds: &Bindings) -> Vec<String> {
    let mut out = Vec::with_capacity(ACTIONS_TABLE.len() + 1);
    out.push("keybindings (use: /keyboard <action> <key>):".into());
    for e in &ACTIONS_TABLE {
        out.push(format_action_binding_line(binds, e));
    }
    out
}

fn format_action_binding_line(binds: &Bindings, e: &ActionTableEntry) -> String {
    let shown = format_bound_chords(binds, e.action);
    let suffix = super::parity::action_parity(e.action).help_suffix();
    format!("  {:<24} {shown}{suffix}", e.config_name)
}

/// Snapshot of default compact help — used by consistency tests and as the committed operator view.
#[must_use]
pub fn default_controls_help() -> String {
    player_controls_help(&Bindings::daoc_defaults())
}

#[cfg(test)]
mod tests {
    use winit::keyboard::KeyCode;

    use super::*;
    use crate::keybinds::Chord;

    #[test]
    fn aliases_resolve_to_canonical_tokens() {
        assert_eq!(canonical_action_token("attack"), "combat_mode");
        assert_eq!(canonical_action_token("ATTACK"), "combat_mode");
        assert_eq!(canonical_action_token("combat_mode"), "combat_mode");
        assert_eq!(canonical_action_token("strafe_left"), "slide_left");
        assert_eq!(
            GameAction::from_name("attack"),
            Some(GameAction::CombatMode)
        );
    }

    #[test]
    fn help_names_combat_mode_never_attack_as_canonical() {
        let help = default_controls_help();
        assert!(
            help.contains("combat_mode"),
            "help must name canonical combat_mode:\n{help}"
        );
        // The word "attack" may appear only as documentation of the legacy alias, not as the
        // printed action column. Compact rows use `combat_mode — combat mode`.
        for line in help.lines() {
            if line.contains("combat_mode") {
                assert!(
                    !line.trim_start().starts_with("attack"),
                    "canonical column must not be attack: {line}"
                );
            }
        }
        assert!(
            help.contains("attack → combat_mode"),
            "help should document the legacy alias:\n{help}"
        );
    }

    #[test]
    fn compact_help_chords_match_runtime_defaults() {
        let binds = Bindings::daoc_defaults();
        let help = compact_controls_lines(&binds).join("\n");
        for &(action, _) in COMPACT_HELP {
            let expected = format_bound_chords(&binds, action);
            assert!(
                help.contains(&expected),
                "help missing live chord `{expected}` for {:?}:\n{help}",
                action
            );
            // Primary table chord must appear for every compact action that has defaults.
            if let Some(primary) = Bindings::table_primary(action) {
                let name = chord_name(primary).expect("primary must name");
                assert!(
                    expected.split(", ").any(|c| c == name),
                    "{:?} defaults must include table primary {name}, got {expected}",
                    action
                );
            }
        }
        // A/D must not be claimed as turn bindings while unbound.
        assert_eq!(binds.action_for(KeyCode::KeyA), None);
        assert_eq!(binds.action_for(KeyCode::KeyD), None);
        assert!(
            help.contains("A/D") && help.contains("unbound"),
            "A/D wording must be honest:\n{help}"
        );
        assert!(
            !help.lines().any(|l| {
                let t = l.to_ascii_lowercase();
                (t.contains("a/d") || t.contains(" a ") || t.starts_with("  a "))
                    && t.contains("turn the character")
            }),
            "stale A/D turn claim must not appear:\n{help}"
        );
    }

    #[test]
    fn rebinding_changes_generated_help() {
        let mut binds = Bindings::daoc_defaults();
        let before = compact_controls_lines(&binds).join("\n");
        assert!(before.contains("F") && before.contains("combat_mode"));

        binds.unbind_action(GameAction::CombatMode);
        binds.bind(KeyCode::KeyR, GameAction::CombatMode);
        let after = compact_controls_lines(&binds).join("\n");
        assert!(
            after.contains("combat_mode") && after.contains("R"),
            "rebound help must show R:\n{after}"
        );
        let combat_line = after
            .lines()
            .find(|l| l.contains("combat_mode"))
            .expect("combat_mode row");
        assert!(
            combat_line.contains('R') && !combat_line.split_whitespace().any(|w| w == "F"),
            "displaced F must leave combat_mode help: {combat_line}"
        );
        assert_ne!(
            before, after,
            "help must change when canonical binding changes"
        );
    }

    #[test]
    fn keyboard_list_agrees_with_bindings_including_modifiers() {
        let binds = Bindings::daoc_defaults();
        let list = keyboard_list_lines(&binds).join("\n");
        assert!(list.contains("toggle_interface"));
        assert!(
            list.contains("Alt+Z"),
            "modifier chords must survive listing:\n{list}"
        );
        assert!(list.contains("combat_mode"));
        assert!(
            !list
                .lines()
                .any(|l| l.contains("combat_mode") && l.contains("attack")),
            "list prints canonical names only"
        );

        let mut rebound = binds.clone();
        rebound.unbind_action(GameAction::JumpUp);
        rebound.bind_chord(Chord::shift(KeyCode::Space), GameAction::JumpUp);
        let listed = keyboard_list_lines(&rebound);
        let jump = listed
            .iter()
            .find(|l| l.contains("jump_up"))
            .expect("jump_up row");
        assert!(
            jump.contains("Shift+Space"),
            "rebound modifier must appear: {jump}"
        );
    }

    #[test]
    fn every_legacy_alias_points_at_a_table_action() {
        for &(alias, canon) in LEGACY_ALIASES {
            let a = GameAction::from_name(alias).unwrap_or_else(|| panic!("alias {alias}"));
            assert_eq!(a.name(), canon, "alias {alias}");
            assert_eq!(GameAction::from_name(canon), Some(a));
        }
        assert_eq!(aliases_for(GameAction::CombatMode), vec!["attack"]);
        assert!(aliases_for(GameAction::SlideLeft).contains(&"strafe_left"));
    }

    /// Compact help is a PRODUCT_WIRED subset; non-Proven rows must show the parity suffix.
    #[test]
    fn compact_help_is_subset_of_product_wired() {
        let binds = Bindings::daoc_defaults();
        let help = compact_controls_lines(&binds).join("\n");
        for &(action, _) in COMPACT_HELP {
            assert!(
                is_product_wired(action),
                "compact help advertises unwired {:?}",
                action
            );
            let parity = super::super::parity::action_parity(action);
            if !parity.is_advertised_implemented() {
                let suffix = parity.help_suffix();
                assert!(
                    !suffix.is_empty()
                        && help.lines().any(
                            |l| l.contains(action.name().as_str()) && l.contains(suffix.trim())
                        ),
                    "non-Proven {:?} must show help suffix in compact lines:\n{help}",
                    action
                );
            }
        }
        let src = include_str!("../bin/rustdaoc.rs");
        assert!(
            !src.contains("Remaining table actions are recognized but not yet wired"),
            "silent wildcard comment must not remain"
        );
        assert!(
            src.contains("product_input::product_input"),
            "rustdaoc must dispatch through product_input, not a GameAction identifier census"
        );
    }
}
