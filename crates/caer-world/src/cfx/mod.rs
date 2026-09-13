//! CFX typed state that would otherwise bloat [`crate::presentation`].
//!
//! Pets / remainder live here. Cast bars, cadence, and combat floaters stay in presentation.

pub mod pets;

pub use pets::{PetOwnership, PetRemainder, PetState, MULTI_PET_RULES, NECRO_BODY_RULES};
