//! Event → logical sound name wiring (System 8 / leg 9).
//!
//! Maps already-decoded game events to client logical names. Playback goes through
//! [`crate::audio::play_logged`] so missing devices stay silent and tests assert *calls*, not ears.

pub mod combat;
pub mod death;
pub mod footstep;
pub mod spell;
pub mod ui;

pub use combat::logical_for_combat_result;
pub use death::{logical_for_player_death, logical_for_player_revive, PLAYER_DEATH_LOGICAL};
pub use footstep::{
    classify_material_base, logical_for_footstep, should_play_footstep, SurfaceKind,
    FOOTSTEP_STRIDE,
};
pub use spell::{logical_for_sound_request, logical_for_spell_cast, logical_for_spell_effect};
pub use ui::{
    logical_for_ui_click, logical_for_ui_rollover, UI_CLICK_LOGICAL, UI_ROLLOVER_LOGICAL,
};
