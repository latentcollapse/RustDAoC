//! Per-action behavioral parity — not a structural ProductInput-presence gate.
//!
//! `PRODUCT_WIRED` only means a typed dispatcher mapping exists. Help and playtest claims
//! must use [`ActionParity`]: only [`ActionParity::Proven`] may be advertised as implemented.

use super::GameAction;

/// Oracle-backed product outcome status for a stock GameAction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionParity {
    /// Discriminating observable matches OPEN_ORACLE / stock client for this action.
    Proven,
    /// Partial path exists (window open, slash send, etc.) without full stock parity proof.
    Partial,
    /// Must not be advertised as working; wrong substitute or missing oracle path.
    Unsupported,
}

impl ActionParity {
    #[must_use]
    pub fn help_suffix(self) -> &'static str {
        match self {
            Self::Proven => "",
            Self::Partial => "  (partial — not stock-parity proven)",
            Self::Unsupported => "  (unsupported — not implemented)",
        }
    }

    #[must_use]
    pub fn is_advertised_implemented(self) -> bool {
        matches!(self, Self::Proven)
    }
}

/// Behavioral parity for every stock table action. Wrong substitutes are [`Unsupported`].
#[must_use]
pub fn action_parity(action: GameAction) -> ActionParity {
    use ActionParity::*;
    match action {
        // Movement / camera — local observable, proven in control_tests.
        GameAction::Forward
        | GameAction::Back
        | GameAction::SlideLeft
        | GameAction::SlideRight
        | GameAction::JumpUp
        | GameAction::Walk
        | GameAction::Sprint
        | GameAction::Forward2
        | GameAction::Back2
        | GameAction::LookUp
        | GameAction::LookDown
        | GameAction::ResetCamera
        | GameAction::CameraToggle
        | GameAction::ToggleNames
        | GameAction::MouseLookToggle
        | GameAction::PanCamera
        | GameAction::MousePan
        | GameAction::Mouse
        | GameAction::PageUp
        | GameAction::PageDown
        | GameAction::PageUpSys
        | GameAction::PageDownSys
        | GameAction::Runlock
        | GameAction::RunLock2 => Proven,

        // Combat / UI / group — Partial until per-action oracle outcome matrix lands (B2 residual).
        GameAction::CombatMode
        | GameAction::Sit
        | GameAction::TargetEnemy
        | GameAction::Destroy
        | GameAction::Chat
        | GameAction::Inventory
        | GameAction::Stats
        | GameAction::Group
        | GameAction::MapWindow
        | GameAction::QuestJournal
        | GameAction::ShowSkills
        | GameAction::CommandWindow
        | GameAction::TargetGroup1
        | GameAction::TargetGroup2
        | GameAction::TargetGroup3
        | GameAction::TargetGroup4
        | GameAction::TargetGroup5
        | GameAction::TargetGroup6
        | GameAction::TargetGroup7
        | GameAction::TargetGroup8
        | GameAction::LastAttacker
        | GameAction::Open
        | GameAction::TargetObject => Partial,

        // Slash / send-only without stock packet parity proof.
        GameAction::Get
        | GameAction::Follow
        | GameAction::Stick
        | GameAction::Face
        | GameAction::Reply
        | GameAction::Consider
        | GameAction::Torch
        | GameAction::NearestLoot
        | GameAction::ShowCharacterInfo
        | GameAction::ShowSpells
        | GameAction::ShowCombat
        | GameAction::Compass
        | GameAction::PerfMeter => Partial,

        // Audit B2 false greens — refuse rather than wrong substitute.
        GameAction::ToggleInterface => Unsupported, // was debug overlay
        GameAction::ChatLog => Unsupported,         // was debug overlay
        GameAction::TakeScreenshot => Unsupported,  // was recorder toggle
        GameAction::UseItem | GameAction::UseItemSecondary => Unsupported, // fixed bag/slot
        GameAction::Sell => Unsupported,            // fixed slot 0
        GameAction::TargetFriend => Unsupported,    // no realm/friend filter
        GameAction::GroundTarget => Unsupported,    // guessed slash; typed 0xEC exists
        GameAction::Craft => Unsupported,           // guessed slash
        GameAction::InformationDelve => Unsupported, // guessed /info
        GameAction::RightHandWeapon | GameAction::TwoHandedWeapon | GameAction::RangedWeapon => {
            Unsupported
        } // unproven slot semantics
        GameAction::RealmWarMap => Unsupported,     // generic map_window fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybinds::ACTIONS_TABLE;

    #[test]
    fn every_table_action_has_explicit_parity() {
        for e in &ACTIONS_TABLE {
            let _ = action_parity(e.action);
        }
        assert_eq!(ACTIONS_TABLE.len(), 74);
    }

    #[test]
    fn b2_false_greens_are_unsupported() {
        for a in [
            GameAction::ToggleInterface,
            GameAction::ChatLog,
            GameAction::TakeScreenshot,
            GameAction::UseItem,
            GameAction::UseItemSecondary,
            GameAction::Sell,
            GameAction::TargetFriend,
            GameAction::GroundTarget,
            GameAction::Craft,
            GameAction::InformationDelve,
            GameAction::RightHandWeapon,
            GameAction::TwoHandedWeapon,
            GameAction::RangedWeapon,
            GameAction::RealmWarMap,
        ] {
            assert_eq!(
                action_parity(a),
                ActionParity::Unsupported,
                "{a:?} must not be advertised as proven"
            );
        }
    }

    #[test]
    fn movement_and_camera_are_proven_windows_are_partial() {
        assert_eq!(action_parity(GameAction::Forward), ActionParity::Proven);
        assert_eq!(
            action_parity(GameAction::CameraToggle),
            ActionParity::Proven
        );
        assert_eq!(action_parity(GameAction::CombatMode), ActionParity::Partial);
        assert_eq!(action_parity(GameAction::Inventory), ActionParity::Partial);
    }
}
