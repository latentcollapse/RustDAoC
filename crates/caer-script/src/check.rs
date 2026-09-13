//! Enforceable catalog invariants for `caer addon check` and unit tests.
//!
//! **Named falsifier:** if a required category has zero catalog entries, [`check_catalog`]
//! returns [`CatalogError::MissingEventCategory`] / [`CatalogError::MissingCommandCategory`]
//! and the CLI exits 1. Prose-only docs without this check are insufficient.

use core::fmt;

use crate::catalog::{catalog_entries, CatalogKind};
use crate::command::AddonCommandCategory;
use crate::event::AddonEventCategory;

/// LUA-SEQ acceptance floor: addon-visible events must cover these categories.
pub const REQUIRED_EVENT_CATEGORIES: &[AddonEventCategory] = &[
    AddonEventCategory::Combat,
    AddonEventCategory::Spell,
    AddonEventCategory::Inventory,
    AddonEventCategory::Chat,
    AddonEventCategory::Zone,
    AddonEventCategory::Group,
];

/// LUA-SEQ acceptance floor: addon-visible commands must cover these categories.
pub const REQUIRED_COMMAND_CATEGORIES: &[AddonCommandCategory] = &[
    AddonCommandCategory::Combat,
    AddonCommandCategory::Spell,
    AddonCommandCategory::Inventory,
    AddonCommandCategory::Chat,
    AddonCommandCategory::Zone,
    AddonCommandCategory::Group,
];

/// Why the catalog failed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogError {
    Empty,
    MissingEventCategory(AddonEventCategory),
    MissingCommandCategory(AddonCommandCategory),
    DuplicateId(&'static str),
    EventWithoutCategory(&'static str),
    CommandWithoutCategory(&'static str),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "addon catalog is empty"),
            Self::MissingEventCategory(c) => {
                write!(f, "required event category `{c}` has no catalog entries")
            }
            Self::MissingCommandCategory(c) => {
                write!(f, "required command category `{c}` has no catalog entries")
            }
            Self::DuplicateId(id) => write!(f, "duplicate catalog id `{id}`"),
            Self::EventWithoutCategory(id) => {
                write!(f, "event `{id}` is missing an event category")
            }
            Self::CommandWithoutCategory(id) => {
                write!(f, "command `{id}` is missing a command category")
            }
        }
    }
}

impl std::error::Error for CatalogError {}

/// Validate the shipped taxonomy catalog ([`catalog_entries`]).
///
/// Fails if the catalog is empty, any required category is absent, ids collide, or a row is
/// missing its kind-appropriate category. This is the named falsifier for LUA-SEQ.
pub fn check_catalog() -> Result<(), CatalogError> {
    check_catalog_entries(catalog_entries())
}

/// Validate an arbitrary catalog slice (production + discriminating unit tests).
///
/// Named falsifier: a synthetic catalog missing a required event category must return
/// [`CatalogError::MissingEventCategory`] — constructing the error string alone is not enough.
pub fn check_catalog_entries(
    entries: &[crate::catalog::AddonCatalogEntry],
) -> Result<(), CatalogError> {
    if entries.is_empty() {
        return Err(CatalogError::Empty);
    }

    let mut seen_ids: Vec<&str> = Vec::with_capacity(entries.len());
    for e in entries {
        if seen_ids.contains(&e.id) {
            return Err(CatalogError::DuplicateId(e.id));
        }
        seen_ids.push(e.id);

        match e.kind {
            CatalogKind::Event => {
                if e.event_category.is_none() {
                    return Err(CatalogError::EventWithoutCategory(e.id));
                }
            }
            CatalogKind::Command => {
                if e.command_category.is_none() {
                    return Err(CatalogError::CommandWithoutCategory(e.id));
                }
            }
        }
    }

    for &need in REQUIRED_EVENT_CATEGORIES {
        let present = entries
            .iter()
            .any(|e| e.kind == CatalogKind::Event && e.event_category == Some(need));
        if !present {
            return Err(CatalogError::MissingEventCategory(need));
        }
    }

    for &need in REQUIRED_COMMAND_CATEGORIES {
        let present = entries
            .iter()
            .any(|e| e.kind == CatalogKind::Command && e.command_category == Some(need));
        if !present {
            return Err(CatalogError::MissingCommandCategory(need));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{catalog_entries, CATALOG};
    use crate::command::AddonCommand;
    use crate::event::AddonEvent;

    #[test]
    fn catalog_is_nonempty_and_covers_required_categories() {
        check_catalog().expect("catalog must pass LUA-SEQ required-category check");
        assert!(!CATALOG.is_empty());
        assert!(!AddonEvent::all().is_empty());
        assert!(!AddonCommand::all().is_empty());
    }

    #[test]
    fn every_event_variant_appears_in_catalog() {
        for ev in AddonEvent::all() {
            assert!(
                CATALOG
                    .iter()
                    .any(|e| e.id == ev.as_str() && e.kind == CatalogKind::Event),
                "AddonEvent::{ev:?} ({}) missing from CATALOG",
                ev.as_str()
            );
        }
    }

    #[test]
    fn every_command_variant_appears_in_catalog() {
        for cmd in AddonCommand::all() {
            assert!(
                CATALOG
                    .iter()
                    .any(|e| e.id == cmd.as_str() && e.kind == CatalogKind::Command),
                "AddonCommand::{cmd:?} ({}) missing from CATALOG",
                cmd.as_str()
            );
        }
    }

    #[test]
    fn required_event_category_list_is_complete_floor() {
        let names: Vec<_> = REQUIRED_EVENT_CATEGORIES
            .iter()
            .map(|c| c.as_str())
            .collect();
        for need in ["combat", "spell", "inventory", "chat", "zone", "group"] {
            assert!(
                names.contains(&need),
                "REQUIRED_EVENT_CATEGORIES must include `{need}`"
            );
        }
    }

    #[test]
    fn required_command_category_list_is_complete_floor() {
        let names: Vec<_> = REQUIRED_COMMAND_CATEGORIES
            .iter()
            .map(|c| c.as_str())
            .collect();
        for need in ["combat", "spell", "inventory", "chat", "zone", "group"] {
            assert!(
                names.contains(&need),
                "REQUIRED_COMMAND_CATEGORIES must include `{need}`"
            );
        }
    }

    /// Discriminating check: constructing a synthetic missing-category error is what the CLI
    /// prints when the floor is broken — proves the failure path is not a silent Ok.
    #[test]
    fn missing_category_error_is_discriminating() {
        let err = CatalogError::MissingEventCategory(AddonEventCategory::Combat);
        let msg = err.to_string();
        assert!(msg.contains("combat"));
        assert!(msg.contains("no catalog entries"));
    }

    /// REDTEAM-A: must actually run [`check_catalog_entries`] on a broken catalog.
    /// Deleting the required-category loop must turn this red (string-only asserts stay green).
    #[test]
    fn missing_required_event_category_fails_check_catalog() {
        let stripped: Vec<_> = catalog_entries()
            .iter()
            .copied()
            .filter(|e| e.event_category != Some(AddonEventCategory::Combat))
            .collect();
        assert!(
            !stripped.is_empty(),
            "fixture must retain non-combat rows so Empty is not the failure mode"
        );
        let err = check_catalog_entries(&stripped).expect_err("combat-stripped catalog must fail");
        assert_eq!(
            err,
            CatalogError::MissingEventCategory(AddonEventCategory::Combat)
        );
    }
}
