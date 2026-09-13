//! Typed combat / magic / status presentation owned by the world model.
//!
//! INT consumes these snapshots for HUD, particles, and audio. This module does **not** draw
//! Atlantis chrome and does not claim particle visual fidelity.

use caer_protocol::combat_anim::CombatAnimation;
use caer_protocol::effects::{ConcentrationEffect, IconEntry};
use caer_protocol::points::CharacterPoints;
use caer_protocol::spells::{InterruptSpellCast, SpellCastAnimation, SpellEffectAnimation};

use crate::CastBarState;

/// Cap on retained combat-result floaters (0xBC only).
pub const MAX_COMBAT_RESULTS: usize = 64;
/// Cap on logical sound requests waiting for the audio lane.
pub const MAX_SOUND_REQUESTS: usize = 64;
/// Cap on icon slots (buff/debuff/CC).
pub const MAX_ICON_SLOTS: usize = 40;
/// Cap on cadence marks (timing instrument — not PNG frames).
pub const MAX_CADENCE_MARKS: usize = 256;

/// How a cast left the wind-up. Chat text cannot produce these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastOutcome {
    /// SpellEffectAnimation 0x1B with success != 0.
    Completed {
        caster_id: u16,
        spell_id: u16,
        target_id: u16,
    },
    /// SpellEffectAnimation 0x1B with success == 0 (resist / fail).
    Failed {
        caster_id: u16,
        spell_id: u16,
        target_id: u16,
    },
    /// InterruptSpellCast 0x73 matching the active caster.
    Interrupted { object_id: u16 },
}

impl CastOutcome {
    #[must_use]
    pub fn is_completed(self) -> bool {
        matches!(self, Self::Completed { .. })
    }
}

/// Cast-bar + outcome snapshot for HUD / UI adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CastPresentation {
    pub self_bar: Option<CastBarState>,
    /// Active 0x72 that is not the local player (peer/NPC). Must not light the local bar.
    pub other_bar: Option<CastBarState>,
    pub outcome: Option<CastOutcome>,
}

/// Logical sound request for the AUD lane. Interrupt/fail must not enqueue [`SoundKind::EffectLand`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoundRequest {
    pub kind: SoundKind,
    pub spell_id: u16,
    pub source_id: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundKind {
    CastWindup,
    EffectLand,
}

/// Presence-only effect request. Resource mapping is placeholder unless a client NIF path is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectRequest {
    pub caster_id: u16,
    pub target_id: u16,
    pub spell_id: u16,
    pub bolt_time: u16,
    pub success: u8,
    pub no_sound: bool,
    pub resource: EffectResource,
}

impl EffectRequest {
    /// Attachment object: target when present, else caster.
    #[must_use]
    pub fn attach_id(self) -> u16 {
        if self.target_id != 0 {
            self.target_id
        } else {
            self.caster_id
        }
    }

    /// Origin object (caster).
    #[must_use]
    pub fn origin_id(self) -> u16 {
        self.caster_id
    }

    /// Packet bolt time in tenths of a second (lifetime / travel hint).
    #[must_use]
    pub fn lifetime_tenths(self) -> u16 {
        self.bolt_time
    }
}

/// Client effect resource. Soft-blob presence is **not** retail fidelity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectResource {
    /// No shipped NIF mapping for this spell_id — presence placeholder only.
    PresencePlaceholder,
    /// A typed client effect identity. Placeholder spawns must not satisfy this.
    Typed { effect_id: u16 },
}

impl EffectResource {
    #[must_use]
    pub fn is_diagnostic_placeholder(self) -> bool {
        matches!(self, Self::PresencePlaceholder)
    }

    /// True only when this resource **is** the typed id. A placeholder never qualifies.
    #[must_use]
    pub fn satisfies_typed(self, effect_id: u16) -> bool {
        matches!(self, Self::Typed { effect_id: id } if id == effect_id)
    }
}

/// Map spell/effect identity → client resource. No invented NIF names.
#[must_use]
pub fn map_effect_resource(_spell_id: u16) -> EffectResource {
    // Provenance gap: no OPEN_ORACLE / OWN_CAPTURE table from spell_id → effects/*.NIF yet.
    EffectResource::PresencePlaceholder
}

/// Falsifier helper: a placeholder spawn must not count as fulfilling `typed_id`.
#[must_use]
pub fn effect_spawn_satisfies_typed(resource: EffectResource, typed_id: u16) -> bool {
    resource.satisfies_typed(typed_id)
}

#[must_use]
pub fn effect_request_from_packet(e: &SpellEffectAnimation) -> EffectRequest {
    EffectRequest {
        caster_id: e.caster_id,
        target_id: e.target_id,
        spell_id: e.spell_id,
        bolt_time: e.bolt_time,
        success: e.success,
        no_sound: e.no_sound,
        resource: map_effect_resource(e.spell_id),
    }
}

/// Combat result for HUD floaters — CombatAnimation 0xBC only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombatResultView {
    pub attacker_id: u16,
    pub defender_id: u16,
    pub result_label: &'static str,
    pub target_health_pct: u8,
    pub hit: bool,
    pub provenance: &'static str,
}

impl CombatResultView {
    #[must_use]
    pub fn from_anim(a: &CombatAnimation) -> Self {
        Self {
            attacker_id: a.attacker_id,
            defender_id: a.defender_id,
            result_label: a.result.label(),
            target_health_pct: a.target_health_pct,
            hit: a.result.is_hit(),
            provenance: a.provenance(),
        }
    }
}

/// OPEN_ORACLE `GameObject.GetConLevel` color band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConBand {
    Grey,
    Green,
    Blue,
    Yellow,
    Orange,
    Red,
    Purple,
}

impl ConBand {
    /// DOL `GetConLevel(viewer, target)`: constep = max(1, (level+9)/10),
    /// con = -(viewer - target) * (1/constep). Bands at ±0.5, ±1.5, ±2.5.
    #[must_use]
    pub fn from_levels(viewer_level: u8, target_level: u8) -> Self {
        let level = i32::from(viewer_level);
        let compare = i32::from(target_level);
        let constep = ((level + 9) / 10).max(1);
        let con = -(level - compare) as f64 / f64::from(constep);
        if con <= -2.5 {
            Self::Grey
        } else if con <= -1.5 {
            Self::Green
        } else if con <= -0.5 {
            Self::Blue
        } else if con < 0.5 {
            Self::Yellow
        } else if con < 1.5 {
            Self::Orange
        } else if con < 2.5 {
            Self::Red
        } else {
            Self::Purple
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Grey => "grey",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Yellow => "yellow",
            Self::Orange => "orange",
            Self::Red => "red",
            Self::Purple => "purple",
        }
    }
}

/// Cadence mark — timing instrument. Stopped-clock PNGs are not this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CadenceMark {
    CastStart { caster_id: u16, spell_id: u16 },
    EffectSuccess { caster_id: u16, spell_id: u16 },
    EffectFail { caster_id: u16, spell_id: u16 },
    Interrupt { object_id: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CadenceSample {
    pub tick: u64,
    pub mark: CadenceMark,
}

/// Monotonic tick log. Call [`CastCadence::advance`] from a real or test clock.
#[derive(Debug, Clone, Default)]
pub struct CastCadence {
    tick: u64,
    samples: Vec<CadenceSample>,
}

impl CastCadence {
    #[must_use]
    pub fn tick(&self) -> u64 {
        self.tick
    }

    pub fn advance(&mut self, ticks: u64) {
        self.tick = self.tick.saturating_add(ticks);
    }

    pub fn observe(&mut self, mark: CadenceMark) {
        if self.samples.len() >= MAX_CADENCE_MARKS {
            self.samples.remove(0);
        }
        self.samples.push(CadenceSample {
            tick: self.tick,
            mark,
        });
    }

    #[must_use]
    pub fn samples(&self) -> &[CadenceSample] {
        &self.samples
    }

    /// Gap in ticks between the first `a` and the first later `b`. `None` if either is missing.
    #[must_use]
    pub fn gap_after(
        &self,
        pred: impl Fn(&CadenceMark) -> bool,
        succ: impl Fn(&CadenceMark) -> bool,
    ) -> Option<u64> {
        let start = self.samples.iter().find(|s| pred(&s.mark))?;
        self.samples
            .iter()
            .filter(|s| s.tick >= start.tick && succ(&s.mark))
            .map(|s| s.tick.saturating_sub(start.tick))
            .next()
    }

    pub fn clear(&mut self) {
        self.samples.clear();
        self.tick = 0;
    }
}

/// Product snapshot INT / HUD / audio consume.
#[derive(Debug, Clone)]
pub struct CombatPresentation<'a> {
    pub cast: CastPresentation,
    pub combat_results: &'a [CombatResultView],
    pub icons: &'a [IconEntry],
    pub concentration: &'a [ConcentrationEffect],
    pub points: Option<CharacterPoints>,
    pub pending_sounds: &'a [SoundRequest],
    pub pending_effects: &'a [crate::ParticleEffectSpawn],
    pub pet: Option<&'a crate::cfx::PetOwnership>,
}

pub(crate) fn push_bounded<T>(q: &mut Vec<T>, item: T, cap: usize) {
    if q.len() >= cap {
        q.remove(0);
    }
    q.push(item);
}

pub(crate) fn sound_for_cast(c: &SpellCastAnimation) -> SoundRequest {
    SoundRequest {
        kind: SoundKind::CastWindup,
        spell_id: c.spell_id,
        source_id: c.caster_id,
    }
}

pub(crate) fn sound_for_effect(e: &SpellEffectAnimation) -> Option<SoundRequest> {
    if e.no_sound || !e.succeeded() {
        return None;
    }
    Some(SoundRequest {
        kind: SoundKind::EffectLand,
        spell_id: e.spell_id,
        source_id: e.caster_id,
    })
}

pub(crate) fn outcome_from_effect(e: &SpellEffectAnimation) -> CastOutcome {
    if e.succeeded() {
        CastOutcome::Completed {
            caster_id: e.caster_id,
            spell_id: e.spell_id,
            target_id: e.target_id,
        }
    } else {
        CastOutcome::Failed {
            caster_id: e.caster_id,
            spell_id: e.spell_id,
            target_id: e.target_id,
        }
    }
}

pub(crate) fn outcome_from_interrupt(i: &InterruptSpellCast) -> CastOutcome {
    CastOutcome::Interrupted {
        object_id: i.object_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn con_bands_match_oracle_even_and_grey() {
        assert_eq!(ConBand::from_levels(50, 50), ConBand::Yellow);
        // constep at 50 is 5: one *con* step is five levels, not one level.
        assert_eq!(ConBand::from_levels(50, 45), ConBand::Blue);
        assert_eq!(ConBand::from_levels(50, 55), ConBand::Orange);
        assert_eq!(ConBand::from_levels(50, 20), ConBand::Grey);
        assert_eq!(ConBand::from_levels(50, 80), ConBand::Purple);
        assert_eq!(ConBand::from_levels(5, 4), ConBand::Blue);
    }

    #[test]
    fn cadence_gap_is_ticks_not_png() {
        let mut c = CastCadence::default();
        c.observe(CadenceMark::CastStart {
            caster_id: 1,
            spell_id: 9,
        });
        c.advance(7);
        c.observe(CadenceMark::Interrupt { object_id: 1 });
        let gap = c
            .gap_after(
                |m| matches!(m, CadenceMark::CastStart { .. }),
                |m| matches!(m, CadenceMark::Interrupt { .. }),
            )
            .expect("gap");
        assert_eq!(gap, 7, "timing instrument must record cadence ticks");
        c.advance(0);
        assert_eq!(c.tick(), 7);
    }

    #[test]
    fn failed_effect_is_not_completed() {
        let e = SpellEffectAnimation {
            caster_id: 1,
            spell_id: 2,
            target_id: 3,
            bolt_time: 0,
            no_sound: false,
            success: 0,
        };
        assert!(!outcome_from_effect(&e).is_completed());
        assert!(sound_for_effect(&e).is_none());
    }

    #[test]
    fn placeholder_does_not_satisfy_typed_effect_id() {
        let placeholder = EffectResource::PresencePlaceholder;
        assert!(placeholder.is_diagnostic_placeholder());
        assert!(
            !effect_spawn_satisfies_typed(placeholder, 407),
            "diagnostic blob must not fulfill typed effect 407"
        );
        let typed = EffectResource::Typed { effect_id: 407 };
        assert!(effect_spawn_satisfies_typed(typed, 407));
        assert!(!effect_spawn_satisfies_typed(typed, 1));
        assert!(!typed.is_diagnostic_placeholder());
    }
}
