//! Product-loop classification for rustdaoc input.
//!
//! [`product_input`] maps advertised [`GameAction`]s. Overlay pointer / Y-N-Esc / invite are
//! constructed directly as [`ProductInput`] variants — rustdaoc must dispatch those through
//! [`crate::product_loop::ProductController`], not call social handlers raw.

use crate::keybinds::GameAction;

/// Edge- or hold-triggered product inputs. Unwired table actions are [`None`].
///
/// Overlay variants (`PointerPress`, `Social*`, `InviteToGroup`) are not GameAction-mapped;
/// rustdaoc builds them from mouse / Y-N-Esc / `/invite`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProductInput {
    HoldForward,
    HoldBack,
    HoldSlideLeft,
    HoldSlideRight,
    JumpUp,
    Chat,
    Destroy,
    TargetEnemy,
    CombatMode,
    Sit,
    ToggleInterface,
    TakeScreenshot,
    CameraToggle,
    ToggleInventory,
    ToggleStats,
    ToggleGroup,
    ToggleMap,
    ToggleQuest,
    ToggleSkills,
    Interact,
    Get,
    Follow,
    Stick,
    Face,
    Walk,
    Sprint,
    TargetFriend,
    TargetObject,
    LastAttacker,
    Reply,
    Consider,
    UseItem,
    UseItemSecondary,
    Sell,
    ShowCombat,
    Runlock,
    GroundTarget,
    TargetGroup1,
    TargetGroup2,
    TargetGroup3,
    TargetGroup4,
    TargetGroup5,
    TargetGroup6,
    TargetGroup7,
    TargetGroup8,
    Craft,
    CommandWindow,
    RightHandWeapon,
    TwoHandedWeapon,
    RangedWeapon,
    RealmWarMap,
    LookUp,
    LookDown,
    ResetCamera,
    ToggleNames,
    MouseLookToggle,
    Torch,
    PerfMeter,
    InformationDelve,
    ChatLog,
    PageUp,
    PageDown,
    PanCamera,
    Mouse,
    /// Skin hit-test pointer press at product window coordinates.
    PointerPress {
        x: f32,
        y: f32,
    },
    /// Overlay Accept (Y) — same typed path as the Accept control.
    SocialAccept,
    /// Overlay Refuse (N) — same typed path as the Refuse control.
    SocialRefuse,
    /// Escape / cancel focused overlay (dialog refuse or trade cancel).
    SocialEscape,
    /// Invite current target (`InviteToGroup` 0x87). Requires a target; never invents membership.
    InviteToGroup,
}

impl ProductInput {
    #[must_use]
    pub fn is_social_ui(self) -> bool {
        matches!(
            self,
            Self::PointerPress { .. }
                | Self::SocialAccept
                | Self::SocialRefuse
                | Self::SocialEscape
                | Self::InviteToGroup
        )
    }
}

#[must_use]
pub fn product_input(action: GameAction) -> Option<ProductInput> {
    Some(match action {
        GameAction::Forward => ProductInput::HoldForward,
        GameAction::Back => ProductInput::HoldBack,
        GameAction::SlideLeft => ProductInput::HoldSlideLeft,
        GameAction::SlideRight => ProductInput::HoldSlideRight,
        GameAction::JumpUp => ProductInput::JumpUp,
        GameAction::Chat => ProductInput::Chat,
        GameAction::Destroy => ProductInput::Destroy,
        GameAction::TargetEnemy => ProductInput::TargetEnemy,
        GameAction::CombatMode => ProductInput::CombatMode,
        GameAction::Sit => ProductInput::Sit,
        GameAction::ToggleInterface => ProductInput::ToggleInterface,
        GameAction::TakeScreenshot => ProductInput::TakeScreenshot,
        GameAction::CameraToggle => ProductInput::CameraToggle,
        GameAction::Inventory => ProductInput::ToggleInventory,
        GameAction::Stats | GameAction::ShowCharacterInfo => ProductInput::ToggleStats,
        GameAction::Group => ProductInput::ToggleGroup,
        GameAction::MapWindow | GameAction::Compass => ProductInput::ToggleMap,
        GameAction::QuestJournal => ProductInput::ToggleQuest,
        GameAction::ShowSkills | GameAction::ShowSpells => ProductInput::ToggleSkills,
        GameAction::Open => ProductInput::Interact,
        GameAction::Get | GameAction::NearestLoot => ProductInput::Get,
        GameAction::Follow => ProductInput::Follow,
        GameAction::Stick => ProductInput::Stick,
        GameAction::Face => ProductInput::Face,
        GameAction::Walk => ProductInput::Walk,
        GameAction::Sprint => ProductInput::Sprint,
        GameAction::TargetFriend => ProductInput::TargetFriend,
        GameAction::TargetObject => ProductInput::TargetObject,
        GameAction::LastAttacker => ProductInput::LastAttacker,
        GameAction::Reply => ProductInput::Reply,
        GameAction::Consider => ProductInput::Consider,
        GameAction::UseItem => ProductInput::UseItem,
        GameAction::UseItemSecondary => ProductInput::UseItemSecondary,
        GameAction::Sell => ProductInput::Sell,
        GameAction::ShowCombat => ProductInput::ShowCombat,
        GameAction::Runlock | GameAction::RunLock2 => ProductInput::Runlock,
        GameAction::GroundTarget => ProductInput::GroundTarget,
        GameAction::TargetGroup1 => ProductInput::TargetGroup1,
        GameAction::TargetGroup2 => ProductInput::TargetGroup2,
        GameAction::TargetGroup3 => ProductInput::TargetGroup3,
        GameAction::TargetGroup4 => ProductInput::TargetGroup4,
        GameAction::TargetGroup5 => ProductInput::TargetGroup5,
        GameAction::TargetGroup6 => ProductInput::TargetGroup6,
        GameAction::TargetGroup7 => ProductInput::TargetGroup7,
        GameAction::TargetGroup8 => ProductInput::TargetGroup8,
        GameAction::Craft => ProductInput::Craft,
        GameAction::CommandWindow => ProductInput::CommandWindow,
        GameAction::RightHandWeapon => ProductInput::RightHandWeapon,
        GameAction::TwoHandedWeapon => ProductInput::TwoHandedWeapon,
        GameAction::RangedWeapon => ProductInput::RangedWeapon,
        GameAction::RealmWarMap => ProductInput::RealmWarMap,
        GameAction::LookUp => ProductInput::LookUp,
        GameAction::LookDown => ProductInput::LookDown,
        GameAction::ResetCamera => ProductInput::ResetCamera,
        GameAction::ToggleNames => ProductInput::ToggleNames,
        GameAction::MouseLookToggle => ProductInput::MouseLookToggle,
        GameAction::Torch => ProductInput::Torch,
        GameAction::PerfMeter => ProductInput::PerfMeter,
        GameAction::InformationDelve => ProductInput::InformationDelve,
        GameAction::ChatLog => ProductInput::ChatLog,
        GameAction::PageUp | GameAction::PageUpSys => ProductInput::PageUp,
        GameAction::PageDown | GameAction::PageDownSys => ProductInput::PageDown,
        GameAction::PanCamera | GameAction::MousePan => ProductInput::PanCamera,
        GameAction::Mouse => ProductInput::Mouse,
        // Duplicate table rows share the primary ProductInput (same wire/UI effect).
        GameAction::Forward2 => ProductInput::HoldForward,
        GameAction::Back2 => ProductInput::HoldBack,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybinds::{
        action_parity, is_product_wired, keyboard_list_lines, ActionParity, Bindings, COMPACT_HELP,
        PRODUCT_WIRED,
    };

    #[test]
    fn every_product_wired_action_has_a_typed_handler() {
        for &action in PRODUCT_WIRED {
            assert!(
                product_input(action).is_some(),
                "PRODUCT_WIRED {:?} has no ProductInput",
                action
            );
        }
        for &(action, _) in COMPACT_HELP {
            assert!(
                is_product_wired(action),
                "compact help advertises unwired {:?}",
                action
            );
            // COMPACT_HELP is a typed-dispatch surface, not a stock-parity claim.  The
            // metadata layer appends an explicit suffix for Partial/Unsupported actions;
            // requiring Proven here would make CombatMode look more complete than its
            // oracle evidence permits.
            assert!(product_input(action).is_some());
        }
        let list = keyboard_list_lines(&Bindings::daoc_defaults()).join("\n");
        assert!(
            list.lines().any(|l| l.contains("forward")
                && !l.contains("unsupported")
                && !l.contains("partial")),
            "forward must be Proven-advertised:\n{list}"
        );
        assert!(
            list.contains("(unsupported"),
            "B2 false greens must be labeled unsupported:\n{list}"
        );
        assert!(
            list.contains("toggle_interface") && list.contains("unsupported"),
            "toggle_interface must not look implemented:\n{list}"
        );
        assert_eq!(
            PRODUCT_WIRED.len(),
            crate::keybinds::TABLE_ACTION_COUNT,
            "PRODUCT_WIRED dispatcher map still covers the 74-action table"
        );
    }

    #[test]
    fn falsifier_removing_a_wired_mapping_breaks_product_input() {
        assert!(product_input(GameAction::Consider).is_some());
        assert!(product_input(GameAction::TargetGroup8).is_some());
        assert!(product_input(GameAction::PageUpSys).is_some());
        assert_eq!(
            product_input(GameAction::Forward2),
            Some(ProductInput::HoldForward)
        );
    }

    #[test]
    fn b2_false_green_actions_remain_mapped_but_unsupported() {
        // Mapping exists so the dispatcher can refuse honestly; parity blocks advertising.
        for a in [
            GameAction::ToggleInterface,
            GameAction::TakeScreenshot,
            GameAction::Sell,
            GameAction::GroundTarget,
        ] {
            assert!(product_input(a).is_some());
            assert_eq!(action_parity(a), ActionParity::Unsupported);
        }
    }
}
