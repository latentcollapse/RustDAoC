//! Typed pet ownership from PetWindow 0x88.
//!
//! OPEN_ORACLE: one controlled oid per packet (`GamePlayer.SetControlledBrain` uses slot 0 and
//! `InitControlledBrainArray(1)`). Charm uses the same sender. Necromancer shade (`Shade(true)`)
//! and Bonedancer commander minion arrays are **not** fields of 0x88 — recorded UNKNOWN.

use caer_protocol::pets::{PetAggro, PetWalk, PetWindow, PetWindowAction};

/// Why a class-specific pet rule is not implemented from this packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PetRemainder {
    /// No OPEN_ORACLE 1.127 field / packet encodes this client-visible rule.
    Unknown,
}

/// Necromancer shade/body swap is a separate `Shade` effect, not PetWindow.
pub const NECRO_BODY_RULES: PetRemainder = PetRemainder::Unknown;
/// Multi-pet windows (Bonedancer minions on the commander) are not a second 0x88 oid.
pub const MULTI_PET_RULES: PetRemainder = PetRemainder::Unknown;

/// Local player's currently owned pet as presented by 0x88.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PetOwnership {
    pub pet_id: u16,
    pub action: PetWindowAction,
    pub aggro: PetAggro,
    pub walk: PetWalk,
    pub icons: Vec<u16>,
    pub provenance: &'static str,
}

impl PetOwnership {
    #[must_use]
    pub fn from_window(w: &PetWindow) -> Option<Self> {
        if w.is_close() {
            return None;
        }
        Some(Self {
            pet_id: w.pet_id,
            action: w.action,
            aggro: w.aggro,
            walk: w.walk,
            icons: w.icons.clone(),
            provenance: w.provenance(),
        })
    }
}

/// Session-scoped pet ownership. Cleared on close, region, logout, and pet ObjectDelete.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PetState {
    owned: Option<PetOwnership>,
}

impl PetState {
    #[must_use]
    pub fn owned(&self) -> Option<&PetOwnership> {
        self.owned.as_ref()
    }

    pub fn apply_window(&mut self, w: &PetWindow) {
        self.owned = PetOwnership::from_window(w);
    }

    pub fn on_object_removed(&mut self, object_id: u16) {
        if self.owned.as_ref().is_some_and(|p| p.pet_id == object_id) {
            self.owned = None;
        }
    }

    pub fn clear(&mut self) {
        self.owned = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caer_protocol::pets::PetWindow;

    #[test]
    fn close_clears_ownership_open_sets_oid() {
        let mut s = PetState::default();
        s.apply_window(&PetWindow {
            pet_id: 44,
            action: PetWindowAction::Open,
            aggro: PetAggro::Defensive,
            walk: PetWalk::Follow,
            icons: vec![1],
        });
        assert_eq!(s.owned().map(|p| p.pet_id), Some(44));
        s.apply_window(&PetWindow {
            pet_id: 0,
            action: PetWindowAction::Close,
            aggro: PetAggro::None,
            walk: PetWalk::None,
            icons: vec![],
        });
        assert!(s.owned().is_none());
    }

    #[test]
    fn necro_and_multi_pet_are_unknown_not_invented() {
        assert_eq!(NECRO_BODY_RULES, PetRemainder::Unknown);
        assert_eq!(MULTI_PET_RULES, PetRemainder::Unknown);
    }
}
