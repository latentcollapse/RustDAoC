//! Live-entity model resolution: turn a creature's numeric model id into an uploaded GPU mesh.
//!
//! Each NPC/object carries a `model` id on the wire. This maps it through the client's
//! `monsters.csv`/`monnifs.csv` tables to a `figures/*.nif`, parses that NIF once, builds an
//! untextured mesh batch, and uploads it to the renderer keyed by model id. The frame loop then
//! draws every entity of that model as instances of the one mesh instead of a placeholder box.
//!
//! Loads are lazy and memoised: the first time a model id is seen in view its mesh is resolved +
//! uploaded; every id (success or failure) is remembered so a missing/broken model is tried once.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use caer_assets::figures::FigureModels;
use caer_assets::monsters::{MonsterModels, SkinSlot};
use caer_assets::pskins::ObjectSkins;
use caer_protocol::customization::{AvatarAppearance, FacialMorphSlot, FACIAL_MORPH_NEUTRAL_TICK};

use crate::gpu::Gpu;
use crate::terrain;

/// Synthetic mesh key for an assembled player avatar of a given (race, gender). Kept well clear of
/// real creature model ids (which are small) so the avatar mesh can share the entity-mesh cache.
pub fn avatar_model_id(race: u8, gender: u8) -> u16 {
    0xF000 | ((race as u16) << 4) | (gender as u16 & 0x0F)
}

/// The hair mesh the figure assembler should retain for one stored hairstyle byte.
///
/// `fig3map` is the owner of hair geometry: it maps the exact one-based Style index to an
/// archive-qualified NIF, with deliberate holes for Bald.  A hair *sheet* merely supplies colour
/// and is frequently shared by several meshes, so deriving geometry from its filename replaces a
/// selected hairstyle with whichever texture happened to be named first.
#[derive(Debug, Clone, PartialEq)]
enum HairMeshSelection {
    /// Stored zero is retail's uncustomized state; keep the base body's first hair part.
    Base,
    /// A concrete, source-indexed hairstyle mesh.
    Source(caer_assets::figures::PartRef),
    /// The selected source value intentionally has no hair mesh (for example `Bald`).
    Omit,
}

fn source_hair_mesh(
    figures: &FigureModels,
    race: u8,
    gender: u8,
    hair_style: u8,
) -> HairMeshSelection {
    if hair_style == 0 {
        return HairMeshSelection::Base;
    }
    figures
        .part_variants(race, gender, caer_assets::figures::PART_HAIR)
        .into_iter()
        .find(|(index, _)| *index == hair_style)
        .map(|(_, part)| HairMeshSelection::Source(part))
        .unwrap_or(HairMeshSelection::Omit)
}

fn is_hair_part(part: &caer_assets::figures::PartRef) -> bool {
    part.filename.to_ascii_lowercase().contains("hair")
}

/// Measured presentation-only adjustments for character-select/create idles.
///
/// This does not choose or replace an animation clip: the caller always keeps the race's own
/// authored clip. The policy only touches channels that have been separately measured against the
/// retail body and rendered reference. `CAER_NO_POSE` and `CAER_FORCE_CLIP` remain diagnostic
/// escape hatches and deliberately return the ordinary rigid policy here.
pub fn character_screen_pose_policy(
    race: u8,
    gender: u8,
) -> Option<caer_assets::nif::PosePolicy<'static>> {
    if std::env::var_os("CAER_NO_POSE").is_some() || std::env::var_os("CAER_FORCE_CLIP").is_some() {
        return None;
    }

    // Canonical biped IDs are the same IDs that KFA tracks address. The explicit arrays make the
    // exception reviewable and prevent a presentation correction from becoming a whole-rig
    // retargeting rule.
    const VALKYN_FEMALE_CLAVICLE_WEIGHT: [(i32, f32); 2] = [(22, 0.70), (35, 0.70)];
    // Spine → neck → clavicles. Never include upper arms, forearms, hands, fingers, thighs, or
    // feet: those keyed translations are a retargeting mismatch and visibly deform Firbolg limbs.
    const FIRBOLG_MALE_UPPER_TORSO: [i32; 6] = [2, 3, 4, 6, 22, 35];

    match (race, gender) {
        (14, caer_assets::figures::GENDER_FEMALE) => Some(caer_assets::nif::PosePolicy {
            translation_ids: &[],
            // `i_vf` is 22.32° from bind at its more pronounced left clavicle. 0.70 yields
            // 15.62° there — the measured relaxed magnitude — without exchanging the Valkyn's
            // hunch for a Briton stance.
            rotation_weights: &VALKYN_FEMALE_CLAVICLE_WEIGHT,
        }),
        (10, caer_assets::figures::GENDER_MALE) => Some(caer_assets::nif::PosePolicy {
            translation_ids: &FIRBOLG_MALE_UPPER_TORSO,
            rotation_weights: &[],
        }),
        _ => None,
    }
}

/// Clip time (seconds) the A.3.4 MVP freezes a creature at — a representative idle frame off the
/// bind T-pose. Clamped to the clip's duration; a fixed frame is enough to prove posing works
/// (per-frame animation is the later locomotion slice).
const POSE_FRAME_T: f32 = 0.5;

/// The clip time the pose report samples at.
///
/// Honours `CAER_ANIM_TIME`, the same variable `--screenshot` uses, so the report can be aligned
/// with the frame it is being read against. It defaults to [`POSE_FRAME_T`] rather than to the
/// screenshot's 0.0 because a report is usually wanted mid-clip, but **a report taken at a different
/// clip time than the picture is describing a different pose** — which is how a first pass at the
/// female head tilt measured t=0.5 against a capture rendered at t=0.
fn pose_report_t(duration: f32) -> f32 {
    std::env::var("CAER_ANIM_TIME")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(POSE_FRAME_T)
        .clamp(0.0, duration)
}

/// Largest per-vertex drift (world units) tolerated between a rigged part's BIND reconstruction and
/// its static mesh before the rig is rejected as "wrong skeleton for this body". A correct external
/// bind lands within ~2 units (measured 0.00–1.79 across the Briton parts); a wrong one is off by
/// tens. Bodies that fail keep their static bind pose rather than render mangled.
const MAX_AVATAR_BIND_ERROR: f32 = 5.0;

/// The bind gate, overridable via `CAER_BIND_GATE` for A/B investigation.
///
/// The gate is a pass/fail on a worst-vertex distance, and "this race fails" says nothing about
/// whether the posed result would actually be wrong. Raising it and rendering is the experiment
/// that distinguishes "the threshold is too strict for this body" from "this body genuinely binds
/// to the wrong skeleton" — the two Firbolg hypotheses, which look identical from the counter.
fn bind_gate() -> f32 {
    std::env::var("CAER_BIND_GATE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(MAX_AVATAR_BIND_ERROR)
}

/// Which locomotion clip an entity is playing.
///
/// Speed alone cannot pick these: walking backwards and sidestepping are the same wire speed as
/// walking forwards, so the DIRECTION of travel relative to the character's facing is what selects
/// them. DAoC separates the two — S walks you backwards and Q/E sidestep, all while you keep facing
/// forward — which is exactly why it needs its own clips rather than a rotated walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Loco {
    Idle,
    Walk,
    Run,
    /// Moving backwards while still facing forward (S).
    Back,
    /// Sidestepping while still facing forward (Q / E).
    SlideLeft,
    SlideRight,
}

/// Which way an entity is travelling relative to the way it is FACING.
///
/// This is the missing input that speed alone can't supply. It is knowable locally for the player
/// (we own the keys) and, for other entities, from the wire's strafe bits — so it is carried as its
/// own value rather than inferred from velocity, which would need frame-to-frame position history.
///
/// A diagonal resolves to whichever component dominates the character's read: forward wins, because
/// running forward-and-slightly-left is a forward run in DAoC, not a sidestep.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Motion {
    #[default]
    Forward,
    Back,
    Left,
    Right,
}

/// The locomotion clips one anim set names, all optional — creature sets are sparse.
#[derive(Default)]
pub struct LocoClips {
    pub walk: Option<caer_assets::nif::Clip>,
    pub run: Option<caer_assets::nif::Clip>,
    pub back: Option<caer_assets::nif::Clip>,
    pub slide_left: Option<caer_assets::nif::Clip>,
    pub slide_right: Option<caer_assets::nif::Clip>,
}

/// How long a locomotion change cross-fades, in seconds.
///
/// **This value is ours, not the client's.** `animnifs.csv` has a `Blend Time` column, but it is
/// `0` in all 3,938 populated rows — the data isn't there, so the duration can't be read from it.
/// 0.2s is short enough to feel responsive and long enough to kill the snap.
pub const BLEND_SECONDS: f32 = 0.2;

/// One entity's animation state, retained across frames so a state change can cross-fade.
/// Per-frame rebuilds can't blend: blending needs to know what you were doing a moment ago.
#[derive(Clone, Copy, Debug)]
pub struct EntityAnim {
    pub cur: Loco,
    /// The state being faded out, and when the fade began. `None` once the blend completes.
    pub prev: Option<Loco>,
    pub blend_start: f32,
    /// Last frame time this entity was seen, so states for departed entities can be pruned.
    pub last_seen: f32,
}

impl EntityAnim {
    /// Weight of the CURRENT state in the cross-fade: 0 → still fully the previous state,
    /// 1 → blend finished. Returns `None` when there's nothing to blend.
    pub fn blend_weight(&self, now: f32) -> Option<f32> {
        let prev = self.prev?;
        if prev == self.cur {
            return None;
        }
        // Compare ELAPSED against the duration rather than testing the ratio against 1.0: in f32
        // the ratio at exactly `blend_start + BLEND_SECONDS` lands on 0.99999905, so a `w < 1.0`
        // test leaves the blend running for an extra frame instead of collapsing to the cheap
        // single-clip path. Caught by `blend_weight_ramps_then_completes`.
        // Tolerance matters here. Testing the ratio against 1.0 leaves the blend running an extra
        // frame, because `blend_start + BLEND_SECONDS` in f32 lands a hair short of the boundary
        // (0.99999905 rather than 1.0). Anything within a millisecond of done IS done — the
        // remaining fraction is invisible and not worth blending two palettes for.
        const DONE_SLACK: f32 = 1e-3;
        let elapsed = now - self.blend_start;
        if elapsed >= BLEND_SECONDS - DONE_SLACK {
            return None;
        }
        Some((elapsed / BLEND_SECONDS).clamp(0.0, 1.0))
    }
}

/// Everything needed to rebuild a GPU-skinned model's bone palette each frame.
///
/// Covers both shapes of skinned mesh: a creature (one rig) and the assembled player avatar
/// (several part rigs sharing one external skeleton), which is why `rigs` is a `Vec`.
pub struct SkinnedRig {
    pub rigs: Vec<caer_assets::nif::RiggedModel>,
    /// Standing idle — the fallback whenever a locomotion clip is missing.
    pub clip: caer_assets::nif::Clip,
    /// Walk / run clips, when the model's anim set has them (A.3.7). Same skeleton, so they share
    /// the palette stride and can be swapped per instance without re-uploading anything.
    pub walk: Option<caer_assets::nif::Clip>,
    pub run: Option<caer_assets::nif::Clip>,
    /// Reverse and sidestep clips (`anims.csv` `Back` / `Slide Left` / `Slide Right`). Backing up
    /// and strafing are not a rotated walk — the character keeps facing forward and moves the other
    /// way, so they are separately authored, and every anim set ships `Back` (425 of 431 ship both
    /// slides).
    pub back: Option<caer_assets::nif::Clip>,
    pub slide_left: Option<caer_assets::nif::Clip>,
    pub slide_right: Option<caer_assets::nif::Clip>,
    /// World units covered by one full cycle of the walk / run clip, from monnifs' `Stride`
    /// columns. Playback rate is `speed / stride` cycles per second, which is what keeps feet
    /// planted instead of skating; `0` means "unknown", and the clip then plays at its authored rate.
    pub stride_walk: f32,
    pub stride_run: f32,
    pub stride_back: f32,
    pub stride_strafe: f32,
    /// Constant Z added to every palette translation. Used to **foot-anchor** the assembled avatar:
    /// fig3 parts are authored with the origin above the feet, and the CPU path fixed that by
    /// shifting baked vertices. Skinned vertices are in skin space and can't be pre-shifted, so the
    /// offset rides on the palette instead — translating every bone shifts the whole body.
    pub z_offset: f32,
}

impl SkinnedRig {
    /// Matrices per instance: every part of every rig, in the same order `skinned_batch_multi`
    /// assigned palette slots.
    pub fn palette_stride(&self) -> usize {
        let bones = self.rigs.first().map_or(0, |r| r.bone_stride());
        self.rigs.iter().map(|r| r.parts.len()).sum::<usize>() * bones
    }

    /// Which clip an entity moving at `speed` should play, and the world units one cycle of it
    /// covers. Falls back to idle whenever the locomotion clip isn't available.
    ///
    /// The walk/run split is by DAoC wire speed: anything at or above `RUN_SPEED` runs. Standing
    /// still is idle, which is the overwhelmingly common case for the mobs on screen.
    pub fn clip_for_speed(&self, speed: u16) -> (&caer_assets::nif::Clip, f32) {
        self.clip_for_state(Self::state_for_speed(speed))
    }

    /// Locomotion state for a wire speed. Split from clip lookup so the runtime can compare states
    /// between frames (to detect a change worth blending) without touching clips.
    pub fn state_for_speed(speed: u16) -> Loco {
        const RUN_SPEED: u16 = 100;
        match speed {
            0 => Loco::Idle,
            s if s >= RUN_SPEED => Loco::Run,
            _ => Loco::Walk,
        }
    }

    /// The clip for a state and the world units one cycle covers, with fallbacks: creature anim
    /// sets are sparse, so a missing walk falls through to run, a missing run to walk, and neither
    /// to idle — a moving creature must still animate rather than freeze.
    pub fn clip_for_state(&self, state: Loco) -> (&caer_assets::nif::Clip, f32) {
        match state {
            Loco::Idle => (&self.clip, 0.0),
            Loco::Run => match (&self.run, &self.walk) {
                (Some(r), _) => (r, self.stride_run),
                (None, Some(w)) => (w, self.stride_walk),
                _ => (&self.clip, 0.0),
            },
            Loco::Walk => match (&self.walk, &self.run) {
                (Some(w), _) => (w, self.stride_walk),
                (None, Some(r)) => (r, self.stride_run),
                _ => (&self.clip, 0.0),
            },
            // A missing directional clip falls through to the forward walk rather than to idle: a
            // character sliding along in its standing pose reads as a bug, whereas one that walks
            // while moving sideways merely reads as unfinished.
            Loco::Back => match (&self.back, &self.walk) {
                (Some(b), _) => (b, self.stride_back),
                (None, Some(w)) => (w, self.stride_walk),
                _ => (&self.clip, 0.0),
            },
            Loco::SlideLeft => match (&self.slide_left, &self.walk) {
                (Some(s), _) => (s, self.stride_strafe),
                (None, Some(w)) => (w, self.stride_walk),
                _ => (&self.clip, 0.0),
            },
            Loco::SlideRight => match (&self.slide_right, &self.walk) {
                (Some(s), _) => (s, self.stride_strafe),
                (None, Some(w)) => (w, self.stride_walk),
                _ => (&self.clip, 0.0),
            },
        }
    }

    /// Locomotion state from a wire speed AND the direction of travel relative to facing.
    ///
    /// Backwards and sidestep beat the forward walk/run split because they are what the player is
    /// actually doing; DAoC has no "run backwards" clip, so any reverse motion is the Back cycle.
    /// A diagonal (forward + strafe) keeps the forward clip, which is what the real client shows.
    #[must_use]
    pub fn state_for_motion(speed: u16, motion: Motion) -> Loco {
        if speed == 0 {
            return Loco::Idle;
        }
        match motion {
            Motion::Forward => Self::state_for_speed(speed),
            Motion::Back => Loco::Back,
            Motion::Left => Loco::SlideLeft,
            Motion::Right => Loco::SlideRight,
        }
    }

    /// Append posed world-bone mat4s only (`bone_stride` per job) — the dynamic half of the
    /// skinning fold. Inverse-bind stays on the GPU; a compute pass finishes the palette.
    pub fn world_bones_of_extend(
        &self,
        clip: &caer_assets::nif::Clip,
        t: f32,
        world: &mut Vec<caer_assets::nif::Xform>,
        out: &mut Vec<[[f32; 4]; 4]>,
    ) {
        let Some(rig) = self.rigs.first() else { return };
        rig.skeleton.pose_into(clip, t, world);
        out.reserve(world.len());
        for w in world.iter() {
            out.push(caer_assets::nif::xform_to_mat4(w));
        }
    }

    /// Blended world bones (locomotion cross-fade) — same contract as [`Self::world_bones_of_extend`].
    pub fn world_bones_blend_extend(
        &self,
        a: &caer_assets::nif::Clip,
        ta: f32,
        b: &caer_assets::nif::Clip,
        tb: f32,
        w: f32,
        world: &mut Vec<caer_assets::nif::Xform>,
        out: &mut Vec<[[f32; 4]; 4]>,
    ) {
        let Some(rig) = self.rigs.first() else { return };
        rig.skeleton.pose_blend_into(a, ta, b, tb, w, world);
        out.reserve(world.len());
        for x in world.iter() {
            out.push(caer_assets::nif::xform_to_mat4(x));
        }
    }

    /// Bones per unique palette (shared skeleton across avatar parts).
    #[must_use]
    pub fn bone_stride(&self) -> usize {
        self.rigs.first().map_or(0, |r| r.bone_stride())
    }

    /// This instance's palette at clip time `t`, with the foot offset folded in.
    pub fn palette(&self, t: f32) -> Vec<[[f32; 4]; 4]> {
        self.palette_of(&self.clip, t)
    }

    /// This instance's palette cross-faded between two clips (locomotion state change).
    pub fn palette_blend(
        &self,
        a: &caer_assets::nif::Clip,
        ta: f32,
        b: &caer_assets::nif::Clip,
        tb: f32,
        w: f32,
    ) -> Vec<[[f32; 4]; 4]> {
        let mut out = Vec::with_capacity(self.palette_stride());
        let mut world = Vec::new();
        self.palette_blend_extend(a, ta, b, tb, w, &mut world, &mut out);
        out
    }

    /// Append a blended palette into `out` using pose scratch `world` (MS-08).
    pub fn palette_blend_extend(
        &self,
        a: &caer_assets::nif::Clip,
        ta: f32,
        b: &caer_assets::nif::Clip,
        tb: f32,
        w: f32,
        world: &mut Vec<caer_assets::nif::Xform>,
        out: &mut Vec<[[f32; 4]; 4]>,
    ) {
        let start = out.len();
        for rig in &self.rigs {
            rig.bone_matrices_blend_extend(a, ta, b, tb, w, world, out);
        }
        self.apply_z_from(out, start);
    }

    /// Like [`Self::palette_blend`] with QA-4 timing (ATTR only).
    pub fn palette_blend_timed(
        &self,
        a: &caer_assets::nif::Clip,
        ta: f32,
        b: &caer_assets::nif::Clip,
        tb: f32,
        w: f32,
        timing: &mut caer_assets::nif::BoneMatrixTiming,
    ) -> Vec<[[f32; 4]; 4]> {
        let mut out = Vec::with_capacity(self.palette_stride());
        let mut world = Vec::new();
        self.palette_blend_timed_extend(a, ta, b, tb, w, timing, &mut world, &mut out);
        out
    }

    /// ATTR blend extend path.
    pub fn palette_blend_timed_extend(
        &self,
        a: &caer_assets::nif::Clip,
        ta: f32,
        b: &caer_assets::nif::Clip,
        tb: f32,
        w: f32,
        timing: &mut caer_assets::nif::BoneMatrixTiming,
        world: &mut Vec<caer_assets::nif::Xform>,
        out: &mut Vec<[[f32; 4]; 4]>,
    ) {
        let start = out.len();
        for rig in &self.rigs {
            rig.bone_matrices_blend_timed_extend(a, ta, b, tb, w, timing, world, out);
        }
        self.apply_z_from(out, start);
    }

    /// Fold the foot-anchor offset into palette translations starting at `from`.
    fn apply_z_from(&self, out: &mut [[[f32; 4]; 4]], from: usize) {
        if self.z_offset != 0.0 {
            for m in out[from..].iter_mut() {
                m[3][2] += self.z_offset;
            }
        }
    }

    /// As [`Self::palette`], for an explicitly chosen clip (locomotion).
    pub fn palette_of(&self, clip: &caer_assets::nif::Clip, t: f32) -> Vec<[[f32; 4]; 4]> {
        let mut out = Vec::with_capacity(self.palette_stride());
        let mut world = Vec::new();
        self.palette_of_extend(clip, t, &mut world, &mut out);
        out
    }

    /// Append a palette into `out` using pose scratch `world` (MS-08 — no per-build Vec).
    pub fn palette_of_extend(
        &self,
        clip: &caer_assets::nif::Clip,
        t: f32,
        world: &mut Vec<caer_assets::nif::Xform>,
        out: &mut Vec<[[f32; 4]; 4]>,
    ) {
        let start = out.len();
        for rig in &self.rigs {
            rig.bone_matrices_extend(clip, t, world, out);
        }
        self.apply_z_from(out, start);
    }

    /// Character-screen palette using an explicit, measured pose policy.
    ///
    /// World locomotion continues to use [`Self::palette_of`] and its rigid-retarget rule. The
    /// policy's allowlist is intentionally carried all the way to the skeleton rather than being
    /// widened to "all translations" at the render boundary.
    pub fn palette_of_with_policy(
        &self,
        clip: &caer_assets::nif::Clip,
        t: f32,
        policy: caer_assets::nif::PosePolicy<'_>,
    ) -> Vec<[[f32; 4]; 4]> {
        let mut out = Vec::with_capacity(self.palette_stride());
        let mut world = Vec::new();
        let start = out.len();
        for rig in &self.rigs {
            rig.bone_matrices_with_policy_extend(clip, t, policy, &mut world, &mut out);
        }
        self.apply_z_from(out.as_mut_slice(), start);
        out
    }

    /// Like [`Self::palette_of`] with QA-4 timing (ATTR only).
    pub fn palette_of_timed(
        &self,
        clip: &caer_assets::nif::Clip,
        t: f32,
        timing: &mut caer_assets::nif::BoneMatrixTiming,
    ) -> Vec<[[f32; 4]; 4]> {
        let mut out = Vec::with_capacity(self.palette_stride());
        let mut world = Vec::new();
        self.palette_of_timed_extend(clip, t, timing, &mut world, &mut out);
        out
    }

    /// ATTR extend path.
    pub fn palette_of_timed_extend(
        &self,
        clip: &caer_assets::nif::Clip,
        t: f32,
        timing: &mut caer_assets::nif::BoneMatrixTiming,
        world: &mut Vec<caer_assets::nif::Xform>,
        out: &mut Vec<[[f32; 4]; 4]>,
    ) {
        let start = out.len();
        for rig in &self.rigs {
            rig.bone_matrices_timed_extend(clip, t, timing, world, out);
        }
        self.apply_z_from(out, start);
    }
}

/// What makes one uploaded avatar mesh different from another: the base model, the armour tiers
/// it wears, the equipment skins bound to it, and the character's own look. The look belongs in
/// the key for the same reason the armour does — two faces on one race are two meshes, and a cache
/// that cannot tell them apart hands back whichever was assembled first, which reads on screen as
/// "customisation does nothing".
type MeshKey = (u16, ArmourTiers, EquipSkinKey, AvatarAppearance);

/// Source facial-morph target IDs form four min/max pairs. The retail head metadata names them
/// `FM1` through `FM8`; target order is deliberately *not* assumed because the NIF puts Eyes
/// before Nose. A nine-tick slider uses the neutral middle (4) as the unmodified base and blends
/// toward exactly one authored target on either side.
fn facial_morph_weight(appearance: AvatarAppearance, id: u8) -> f32 {
    let (slot, low_id, high_id) = match id {
        1 | 2 => (FacialMorphSlot::Nose, 1, 2),
        3 | 4 => (FacialMorphSlot::Eyes, 3, 4),
        5 | 6 => (FacialMorphSlot::LipsOrEars, 5, 6),
        7 | 8 => (FacialMorphSlot::JawOrChin, 7, 8),
        _ => return 0.0, // FM9+ are source-owned non-slider targets; do not invent a control.
    };
    let tick = appearance.facial_morph_tick(slot);
    let neutral = FACIAL_MORPH_NEUTRAL_TICK;
    if id == low_id && tick < neutral {
        f32::from(neutral - tick) / f32::from(neutral)
    } else if id == high_id && tick > neutral {
        f32::from(tick - neutral) / f32::from(neutral)
    } else {
        0.0
    }
}

/// Apply the static, source-tagged player-head blend targets before GPU skinning.
///
/// The base figure's `NiGeomMorpherController` uses the same container as animated scenery, but
/// the head's 0.00001-second base key is a Gamebryo sentinel and its eight facial targets are
/// relative offsets in skin space. Applying them here keeps the existing skinning palette and
/// makes a distinct source appearance a distinct uploaded mesh. Timed/untagged controllers are
/// left entirely alone.
fn apply_facial_morph_targets(
    positions: &mut [[f32; 3]],
    morph: Option<&caer_assets::nif::MorphAnim>,
    target_ids: &[Option<u8>],
    appearance: AvatarAppearance,
) {
    if !appearance.facial_morphs_enabled {
        return;
    }
    let Some(anim) = morph else {
        return;
    };
    if anim.is_animated() || !anim.has_static_targets() || !anim.relative {
        return;
    }
    for (target_index, target) in anim.targets.iter().enumerate().skip(1) {
        let Some(id) = target_ids.get(target_index).copied().flatten() else {
            continue;
        };
        let weight = facial_morph_weight(appearance, id);
        if weight <= 0.0 {
            continue;
        }
        for (position, delta) in positions.iter_mut().zip(&target.deltas) {
            position[0] += delta[0] * weight;
            position[1] += delta[1] * weight;
            position[2] += delta[2] * weight;
        }
    }
}

fn apply_facial_morphs(rig: &mut caer_assets::nif::RiggedModel, appearance: AvatarAppearance) {
    for part in &mut rig.parts {
        apply_facial_morph_targets(
            &mut part.positions,
            part.morph.as_ref(),
            &part.morph_target_ids,
            appearance,
        );
    }
}

/// Apply the same source-tagged blend data to a bind-pose fallback.  A rig failing its measured
/// skeleton gate must not silently lose the selected nose/jaw simply because it uses the static
/// renderer path instead of GPU skinning.
fn apply_facial_morphs_to_model(model: &mut caer_assets::nif::Model, appearance: AvatarAppearance) {
    for part in &mut model.parts {
        apply_facial_morph_targets(
            &mut part.positions,
            part.morph.as_ref(),
            &part.morph_target_ids,
            appearance,
        );
    }
}

pub struct EntityModels {
    /// model id → figures NIF base name.
    resolver: MonsterModels,
    /// Player-avatar (fig3) resolver: (race, gender) → base-body part NIF references.
    figure_resolver: Option<FigureModels>,
    /// `figures/` case-insensitive index: lowercased stem (no extension) → file path.
    figures: HashMap<String, PathBuf>,
    /// `anims/` case-insensitive index: lowercased stem (no extension) → `.kfa` path. Used to resolve
    /// a creature's idle clip so its mesh is posed instead of drawn in the authored T-pose (A.3.4).
    anims: HashMap<String, PathBuf>,
    /// The client's animation tables (`anims`/`canims`/`animnifs`) — the authoritative
    /// anim-set → action → `.kfa` resolution that replaced A.3.4's NIF-stem guess (A.3.5).
    anim_tables: Option<caer_assets::anims::AnimTables>,
    /// `gamedata.mpk : mskins.csv` — the client's index of which skins it ships and which archive
    /// each lives in. `None` only if gamedata could not be read, in which case the binder falls back
    /// to probing archives by constructed name, which is what it used to do always.
    mskins: Option<caer_assets::mskins::Mskins>,
    /// Lazy `figures/Mskins/*.mpk` index: lowercased DDS name → owning archive. Built on first
    /// avatar texture lookup; see [`avatar_texture`].
    mskin_index: Option<HashMap<String, PathBuf>>,
    /// `figures/skins` member -> archive, built on first use like `mskin_index`.
    skin_index: Option<HashMap<String, PathBuf>>,
    /// The sheets last handed to `upload_skinned_mesh`, keyed exactly as the batch references them
    /// (`#opaque` suffix included). Kept so a diagnostic can read the texture the GPU got rather
    /// than re-decoding the archive: those differ wherever the binder processed a sheet, and a
    /// report describing the file while claiming to describe the screen sent an A5 diagnosis the
    /// wrong way. One avatar's worth, replaced on each build.
    last_avatar_textures: HashMap<String, caer_assets::dds::DdsTexture>,
    /// Equipment model → player armour skins (`objects.csv` × `pskins.csv`). `None` if gamedata
    /// lacked those tables — equipment then keeps the creature/white fallback.
    object_skins: Option<ObjectSkins>,
    fig3_look: Option<caer_assets::fig3_look::Fig3Look>,
    /// `figures/fig3/sfig*.mpk` paths — the skeleton archives, a separate family from the `fig*`
    /// part archives. Searched for a race's `<token>_<m|f>_skeleton.nif` when assembling an avatar.
    skeleton_archives: Vec<PathBuf>,
    /// Rig + idle clip retained for every GPU-skinned model, so palettes can be rebuilt each frame
    /// at each instance's own clip time. The CPU path needed none of this — it baked one pose at
    /// load — which is exactly why every instance was frozen together.
    rigs: HashMap<u16, SkinnedRig>,
    /// Per-entity animation state (object_id → state), retained across frames so locomotion
    /// changes cross-fade instead of snapping. Pruned when entities stop being seen.
    anim_states: HashMap<u16, EntityAnim>,
    /// Model ids already attempted, so a resolve/parse failure is never retried each frame.
    /// Keyed by (model, armour tiers, equipment skin key) so two NPCs sharing a NIF but wearing
    /// different plate upload distinct meshes.
    tried: HashSet<MeshKey>,
    /// GPU mesh id → the resolver key that can rebuild it. World rendering retains only meshes
    /// visible this frame; evicting an old one must also release this key from `tried` so returning
    /// to that area can load it again.
    resident_meshes: HashMap<u16, MeshKey>,
    /// Assembled height (bind pose, foot to crown) of every avatar/entity mesh uploaded so far.
    ///
    /// Exists so a camera can be composed around a body instead of around a guessed constant —
    /// the character screen needs to know how tall the figure it is framing actually is.
    heights: HashMap<u16, f32>,
    /// Non-default armour variants → synthetic GPU mesh id (`0xE000`+).
    equipped_ids: HashMap<MeshKey, u16>,
    equipped_order: VecDeque<MeshKey>,
    next_equipped_id: u16,
    last_stand: AvatarStandInfo,
    last_stands: HashMap<u16, AvatarStandInfo>,
}

/// What the last [`EntityModels::ensure_avatar`] actually bound — Gate 0 for white bodies.
#[derive(Clone, Debug, Default)]
pub struct AvatarStandInfo {
    pub fig3_parts: usize,
    pub textured: usize,
    pub eq_bound: usize,
    pub mskin_bound: usize,
    pub path: &'static str,
    pub part_names: String,
    /// `(mesh part, bind error)` for every part the rig gate measured.
    ///
    /// The gate is pass/fail at `MAX_AVATAR_BIND_ERROR`, but the *distribution* is the diagnostic:
    /// a race whose parts all sit near 0.0 with one outlier has one bad variant, while a race
    /// whose parts are uniformly huge is bound to the wrong skeleton entirely. Without the numbers
    /// those two look identical from outside — both just render white and unposed.
    pub bind_errors: Vec<(String, f32)>,
    /// `(mesh part, texture actually bound)` for every part, in draw order.
    ///
    /// A count of bound textures cannot tell a Highlander's tartan from generic cloth — both
    /// score "5 of 5 bound". Two wrong binders shipped behind that number. The names are what
    /// distinguishes a correct skin from merely *a* skin, so the report carries them.
    pub bound_textures: Vec<(String, String)>,
}

impl EntityModels {
    /// Load the resolver tables from `gamedata.mpk` and index the loose `figures/` NIFs. Returns
    /// `None` if the client assets aren't present — the renderer then just keeps drawing boxes.
    pub fn load() -> Option<Self> {
        let root = terrain::client_root();
        let resolver = MonsterModels::load(root.join("gamedata.mpk")).ok()?;
        let figure_resolver = FigureModels::load(root.join("gamedata.mpk")).ok();
        let object_skins = ObjectSkins::load(root.join("gamedata.mpk")).ok();
        let fig3_look = caer_assets::fig3_look::Fig3Look::load(root.join("gamedata.mpk")).ok();
        let mut figures = HashMap::new();
        if let Ok(rd) = std::fs::read_dir(root.join("figures")) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("nif")) {
                    if let Some(stem) = p.file_stem() {
                        figures.insert(stem.to_string_lossy().to_ascii_lowercase(), p);
                    }
                }
            }
        }
        let mut anims = HashMap::new();
        if let Ok(rd) = std::fs::read_dir(root.join("anims")) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("kfa")) {
                    if let Some(stem) = p.file_stem() {
                        anims.insert(stem.to_string_lossy().to_ascii_lowercase(), p);
                    }
                }
            }
        }
        // Keep the renderer and every skeleton-analysis tool on the same ordered archive surface.
        // A duplicate member can be unreadable in one archive and parseable in the next.
        let skeleton_archives = caer_assets::figures::skeleton_archives(&root).unwrap_or_default();
        let anim_tables = caer_assets::anims::AnimTables::load(root.join("gamedata.mpk")).ok();
        let mskins = caer_assets::mskins::Mskins::load(root.join("gamedata.mpk")).ok();
        log::info!(
            "caer-render: entity models — {} ids resolvable ({} animated), {} figures NIFs, {} anim clips indexed, {} anim sets, {} skeleton archives",
            resolver.len(),
            resolver.animated_len(),
            figures.len(),
            anims.len(),
            anim_tables.as_ref().map_or(0, |t| t.len()),
            skeleton_archives.len(),
        );
        Some(Self {
            resolver,
            figure_resolver,
            last_avatar_textures: HashMap::new(),
            figures,
            anims,
            anim_tables,
            mskins,
            mskin_index: None,
            skin_index: None,
            object_skins,
            fig3_look,
            skeleton_archives,
            rigs: HashMap::new(),
            anim_states: HashMap::new(),
            tried: HashSet::new(),
            resident_meshes: HashMap::new(),
            heights: HashMap::new(),
            equipped_ids: HashMap::new(),
            equipped_order: VecDeque::new(),
            next_equipped_id: 0xE000,
            last_stand: AvatarStandInfo::default(),
            last_stands: HashMap::new(),
        })
    }

    /// A creature's walk/run clips from its anim set, when both table and file resolve.
    fn locomotion_clips(&self, model_id: u16) -> LocoClips {
        let Some(set) = self.resolver.anim_set(model_id) else {
            return LocoClips::default();
        };
        self.loco_clips_for_set(set)
    }

    /// Load every locomotion clip an anim set names. Shared by the creature and avatar paths, which
    /// differ only in how they find the set id.
    fn loco_clips_for_set(&self, set: u16) -> LocoClips {
        use caer_assets::anims::Action;
        let Some(tables) = self.anim_tables.as_ref() else {
            return LocoClips::default();
        };
        let get = |a| tables.clip(set, a).and_then(|c| self.load_clip(c));
        LocoClips {
            walk: get(Action::Walk),
            run: get(Action::Run),
            back: get(Action::Back),
            slide_left: get(Action::SlideLeft),
            slide_right: get(Action::SlideRight),
        }
    }

    /// The same, for a player avatar (anim set keyed by race + gender rather than model id).
    fn avatar_locomotion_clips(&self, race: u8, gender: u8) -> LocoClips {
        let Some(tables) = self.anim_tables.as_ref() else {
            return LocoClips::default();
        };
        let gw = if gender == caer_assets::figures::GENDER_FEMALE {
            "Female"
        } else {
            "Male"
        };
        let set = caer_assets::figures::FigureModels::race_name(race)
            .and_then(|r| tables.set_for_race_gender(r, gw))
            .or_else(|| tables.set_for_race_gender("Briton", gw));
        let Some(set) = set else {
            return LocoClips::default();
        };
        self.loco_clips_for_set(set)
    }

    /// Advance one entity's animation state and return it. Starts a cross-fade when the
    /// locomotion state changes; carries an in-progress fade through unchanged.
    pub fn tick_anim(&mut self, object_id: u16, want: Loco, now: f32) -> EntityAnim {
        let st = self.anim_states.entry(object_id).or_insert(EntityAnim {
            cur: want,
            prev: None,
            blend_start: now,
            last_seen: now,
        });
        if st.cur != want {
            // Fade from whatever is on screen right now. If a fade was already running we take its
            // current state as the new source, which is a slight approximation but avoids keeping
            // a chain of blends alive.
            st.prev = Some(st.cur);
            st.cur = want;
            st.blend_start = now;
        }
        st.last_seen = now;
        *st
    }

    /// Drop animation state for entities not seen recently, so the map can't grow without bound as
    /// mobs stream in and out of view.
    pub fn prune_anim_states(&mut self, now: f32) {
        const STALE_SECONDS: f32 = 5.0;
        self.anim_states
            .retain(|_, a| now - a.last_seen < STALE_SECONDS);
    }

    /// Immediate anim-ownership release (ObjectRemoved). Time-based prune is not a substitute.
    pub fn release_anim(&mut self, object_id: u16) -> bool {
        self.anim_states.remove(&object_id).is_some()
    }

    #[must_use]
    pub fn has_anim(&self, object_id: u16) -> bool {
        self.anim_states.contains_key(&object_id)
    }

    /// Drop locomotion state for object ids no longer in [`caer_world::WorldState`].
    pub fn release_anim_not_in_world(&mut self, world: &caer_world::WorldState) {
        self.anim_states.retain(|id, _| world.get(*id).is_some());
    }

    /// The retained rig set for a GPU-skinned model, for per-frame palette building.
    pub fn skinned_rig(&self, model_id: u16) -> Option<&SkinnedRig> {
        self.rigs.get(&model_id)
    }

    /// Whether a creature model id resolves to a `figures/` NIF that is actually present. This is
    /// the resolution layer the renderer sits on: when it breaks, creatures silently fall back to
    /// placeholder boxes — a failure that still "renders fine", so it needs asserting.
    pub fn has_model(&self, model_id: u16) -> bool {
        self.resolver
            .nif_name(model_id)
            .is_some_and(|n| self.figures.contains_key(n))
    }

    /// Load a clip the animation tables resolved, from the `anims/` index. `None` if the client
    /// doesn't ship that `.kfa` (the tables reference some clips no longer present) or it won't parse.
    fn load_clip(&self, cref: &caer_assets::anims::AnimClipRef) -> Option<caer_assets::nif::Clip> {
        let path = self.anims.get(&cref.stem)?;
        let bytes = std::fs::read(path).ok()?;
        let mut clip = caer_assets::nif::read_clip(&bytes).ok()?;
        // Attach the table's playback rate. The .kfa only knows its authored key timeline, so
        // dropping `cref` here is what left every clip running at its authoring rate.
        let r = cref.rate_scale();
        clip.rate = if r > 0.0 { r } else { 1.0 };
        Some(clip)
    }

    /// Resolve the clip to pose a creature in: its table-driven `Idle`, falling back to `CombatIdle`
    /// and finally to the A.3.4 NIF-stem heuristic so no creature regresses to a T-pose.
    fn resolve_pose_clip(&self, model_id: u16, nif_stem: &str) -> Option<caer_assets::nif::Clip> {
        if let (Some(tables), Some(set)) =
            (self.anim_tables.as_ref(), self.resolver.anim_set(model_id))
        {
            for action in [
                caer_assets::anims::Action::Idle,
                caer_assets::anims::Action::CombatIdle,
            ] {
                if let Some(cref) = tables.clip(set, action) {
                    if let Some(clip) = self.load_clip(cref) {
                        return Some(clip);
                    }
                }
            }
        }
        self.resolve_idle_clip_by_stem(nif_stem)
    }

    /// A.3.4's NIF-stem guess, kept only as the last-resort fallback for models the tables don't
    /// cover: strip a trailing model number (`skel01` → `skel`) and look for `<stem>_cidle`/`_idle`.
    /// Superseded by [`Self::resolve_pose_clip`] — the tables are authoritative and often disagree
    /// (skel01's real idle is the shared `I_hm.kfa`, not `skel_CIDLE.kfa`).
    fn resolve_idle_clip_by_stem(&self, nif_stem: &str) -> Option<caer_assets::nif::Clip> {
        let base = nif_stem.trim_end_matches(|c: char| c.is_ascii_digit());
        let base = if base.is_empty() { nif_stem } else { base };
        let low = base.to_ascii_lowercase();
        // Try the creature-specific stem, then the exact NIF stem, across the idle suffixes.
        for stem in [low.as_str(), &nif_stem.to_ascii_lowercase()] {
            for suffix in ["_cidle", "_idle"] {
                if let Some(path) = self.anims.get(&format!("{stem}{suffix}")) {
                    if let Ok(bytes) = std::fs::read(path) {
                        if let Ok(clip) = caer_assets::nif::read_clip(&bytes) {
                            return Some(clip);
                        }
                    }
                }
            }
        }
        None
    }

    /// Resolve the player avatar's idle clip: race id + gender → the `anims.csv` row name
    /// ("Briton Male") → anim set → `Idle`. Falls back to `CombatIdle`, then `None` (bind pose).
    fn resolve_avatar_clip(&self, race: u8, gender: u8) -> Option<caer_assets::nif::Clip> {
        if std::env::var_os("CAER_NO_POSE").is_some() {
            return None; // A/B control: fall back to the static bind-pose assembly.
        }
        let tables = self.anim_tables.as_ref()?;
        // CAER_FORCE_CLIP=<stem> plays one named clip on every avatar, so a pose can be attributed
        // to the clip or to us without editing the anim tables.
        //
        // The stem resolves through its own table row: a hardcoded rate is correct only at t=0 and
        // phase-shifts any timed comparison. An unusable override falls through to normal
        // selection and says so on stdout — returning `None` would drop the figure to bind pose,
        // so a bad stem would silently change the render and read as a null result in the very
        // comparison this flag exists to run.
        if let Some(stem) = std::env::var_os("CAER_FORCE_CLIP") {
            let stem = stem.to_string_lossy().to_ascii_lowercase();
            match tables
                .clip_by_stem(&stem)
                .cloned()
                .and_then(|c| self.load_clip(&c))
            {
                Some(clip) => {
                    println!(
                        "rustdaoc: CAER_FORCE_CLIP={stem} applied ({} tracks)",
                        clip.tracks.len()
                    );
                    return Some(clip);
                }
                None => println!(
                    "rustdaoc: CAER_FORCE_CLIP={stem} unusable — ignoring it and resolving normally"
                ),
            }
        }
        let race_name = caer_assets::figures::FigureModels::race_name(race)?;
        let gender_word = if gender == caer_assets::figures::GENDER_FEMALE {
            "Female"
        } else {
            "Male"
        };
        // Try the race's own set, then the canonical human rows ("Briton <Gender>", sets 2/6).
        //
        // The fallback was introduced when the Catacombs-era `cat_*` clips wouldn't parse, forcing
        // four races to borrow `i_hm`/`i_hf`. Gamebryo 10.1 clip support landed since, so **every
        // race now resolves its own authored idle** and nothing reaches the fallback in practice.
        // It stays as a cheap safety net: a clip the client ships but we can't read should cost a
        // race its exact animation, not leave it T-posed.
        let mut sets = vec![tables.set_for_race_gender(race_name, gender_word)];
        sets.push(tables.set_for_race_gender("Briton", gender_word));
        for set in sets.into_iter().flatten() {
            for action in [
                caer_assets::anims::Action::Idle,
                caer_assets::anims::Action::CombatIdle,
            ] {
                if let Some(cref) = tables.clip(set, action) {
                    if let Some(clip) = self.load_clip(cref) {
                        return Some(clip);
                    }
                }
            }
        }
        None
    }

    /// [`posed_head_height`] for an assembled avatar, or `None` if it has no skinned rig.
    #[must_use]
    pub fn avatar_head_height(&self, model_id: u16, t: f32, scale: f32) -> Option<f32> {
        let rig = self.skinned_rig(model_id)?;
        posed_head_height(
            &rig.rigs.first()?.skeleton,
            &rig.clip,
            t,
            scale,
            rig.z_offset,
        )
    }
}

/// Rendered height of the posed `Bip01 Head` above the instance anchor.
///
/// `(posed head Z + z_offset) * scale`. The offset comes first because palette anchoring precedes
/// the instance scale in the skinning path — see [`SkinnedRig::z_offset`].
///
/// The pre-world camera aims here. A fraction of the mesh height cannot substitute: the eye would
/// be a fraction of the UNSCALED `model_height` while the head is a real scaled position, leaving
/// an error that grows with instance scale. Ledger E13 carries the measurements.
///
/// Lives here, not in `rustdaoc`, so the product path and its gate call the same owner.
#[must_use]
pub fn posed_head_height(
    skeleton: &caer_assets::nif::Skeleton,
    clip: &caer_assets::nif::Clip,
    t: f32,
    scale: f32,
    z_offset: f32,
) -> Option<f32> {
    let i = skeleton
        .bones
        .iter()
        .position(|b| b.name.eq_ignore_ascii_case("Bip01 Head"))?;
    Some((skeleton.pose(clip, t).get(i)?.2[2] + z_offset) * scale)
}

impl EntityModels {
    /// A race+gender's authored skeleton: `figures/fig3/sfigNNN.mpk : <token>_<m|f>_skeleton.nif`
    /// (Briton Male → `bri_m_skeleton.nif`, 129 bones). All 18 playable races ship one. The `sfig*`
    /// archives are a separate family from the `fig*` part archives, which is why scanning only the
    /// body parts found a rig for Briton alone.
    fn race_skeleton(&self, race: u8, gender: u8) -> Option<caer_assets::nif::Skeleton> {
        let want = caer_assets::figures::FigureModels::skeleton_member(race, gender)?;
        let lookup = caer_assets::figures::first_parseable_skeleton(&self.skeleton_archives, &want);
        for (archive, error) in &lookup.rejected_archives {
            // Keep looking. The same member ships in more than one archive and the copies can
            // differ in incidental controller data: `sfig001` carries a `Fir_*_skeleton.nif`
            // with a leftover Max lighting rig while `sfig002` carries a cleaner 129-bone copy.
            // Both Firbolg copies parse now that `NiLightColorController` is read correctly, but
            // another figure must never fall back to `common_body_skeleton` merely because the
            // first archive is richer than the next one.
            log::debug!(
                "caer-render: skeleton {want} in {} unreadable ({error}) — trying the next archive",
                archive.display()
            );
        }
        if lookup.skeleton.is_none() {
            log::debug!("caer-render: no skeleton {want} in any sfig archive for race {race}");
        }
        lookup.skeleton
    }

    /// Last-resort skeleton: the `bcommonm`/`bcommonf` "common body" mesh. NOT the authored rig —
    /// binding fig3 parts to it drifts by ~66 units, so in practice every part fails the bind check
    /// and the body renders static. Kept only so a figure whose race token doesn't resolve still has
    /// something to try; [`Self::race_skeleton`] is the real source.
    fn common_body_skeleton(&self, race: u8, gender: u8) -> Option<caer_assets::nif::Skeleton> {
        let gender_word = if gender == caer_assets::figures::GENDER_FEMALE {
            "Female"
        } else {
            "Male"
        };
        // Prefer the race's own monnifs row if one exists; otherwise fall back to the gender's
        // canonical common body (the only rigs the client ships).
        let by_race = caer_assets::figures::FigureModels::race_name(race)
            .and_then(|r| self.resolver.nif_for_label(&format!("{r} {gender_word}")));
        let nif = by_race
            .or_else(|| {
                self.resolver
                    .nif_for_label(&format!("Briton {gender_word}"))
            })
            .unwrap_or(if gender == caer_assets::figures::GENDER_FEMALE {
                "bcommonf"
            } else {
                "bcommonm"
            });
        let path = self.figures.get(nif)?;
        caer_assets::nif::read_skeleton(&std::fs::read(path).ok()?).ok()
    }

    /// Ensure the assembled player-avatar mesh for `(race, gender)` is uploaded to `gpu` (once),
    /// returning its synthetic model id if a body could be built, else `None` (caller keeps the box).
    /// The body is the fig3 base parts (Head/Body/LBody/Legs/Arms/Hair) loaded from their
    /// `figures/fig3/figNNN.mpk` archives, parsed, and merged into a single mesh.
    /// Assemble the player's fig3 avatar. When `equip` is set, binds authored pskin textures from
    /// the equipped object models (MS-02b) so armour reads as armour on `Self_`, not a white body.
    pub fn ensure_avatar(
        &mut self,
        gpu: &mut Gpu,
        race: u8,
        gender: u8,
        appearance: impl Into<AvatarAppearance>,
        equip: Option<&caer_protocol::equipment::EquipmentUpdate>,
    ) -> Option<u16> {
        let appearance = appearance.into();
        let custom = appearance.customization;
        let base_id = avatar_model_id(race, gender);
        // Fig3 base_body does not yet select armour-tier part variants — texture swap is the MS-02b
        // lever. Cache still keys on EquipSkinKey so studded vs plate upload distinct GPU meshes.
        let skin_key = equip.map(EquipSkinKey::from_equipment).unwrap_or_default();
        let tiers = ArmourTiers::default();
        let empty_fig3 = self
            .figure_resolver
            .as_ref()
            .map(|r| r.base_body(race, gender).is_empty())
            .unwrap_or(true);
        if empty_fig3 {
            // LotM Minotaurs (and any race fig3 does not name) are living-model meshes.
            let wire_gender = gender.saturating_sub(1);
            if let Some(mid) = caer_protocol::career::race_model(race, wire_gender) {
                log::debug!(
                    "caer-render: avatar — race {race} gender {gender} has no fig3 body; using living model {mid}"
                );
                let id = self.ensure_mesh_for_equipment(gpu, mid, equip);
                self.record_stand(
                    id.unwrap_or(mid),
                    AvatarStandInfo {
                        fig3_parts: 0,
                        textured: usize::from(id.is_some()),
                        eq_bound: 0,
                        mskin_bound: 0,
                        path: "living",
                        part_names: format!("living:{mid}"),
                        bind_errors: Vec::new(),
                        bound_textures: Vec::new(),
                    },
                );
                return id;
            }
            return None;
        }
        let id = self.gpu_id_for(gpu, base_id, tiers, skin_key, appearance);
        if gpu.has_entity_mesh(id) || gpu.has_skinned_mesh(id) {
            if let Some(info) = self.last_stands.get(&id) {
                self.last_stand = info.clone();
            }
            return Some(id);
        }
        // First attempt only; a failure stays failed (don't re-open archives every frame).
        if !self.tried.insert((base_id, tiers, skin_key, appearance)) {
            return None;
        }
        let resolver = self.figure_resolver.as_ref()?;
        // MS-02b-full: helm/cloak are fig3 equipment meshes (part idx 9 / 7), not base-body.
        let wear_helm = equip
            .and_then(|e| e.item(caer_protocol::equipment::slot::HELM))
            .is_some();
        let wear_cloak = equip
            .and_then(|e| e.item(caer_protocol::equipment::slot::CLOAK))
            .is_some();
        #[derive(Clone, Copy)]
        enum EquipMesh {
            Base,
            Helm,
            Cloak,
        }
        let mut parts: Vec<(EquipMesh, caer_assets::figures::PartRef)> = resolver
            .base_body(race, gender)
            .into_iter()
            .filter(|p| {
                if !wear_helm {
                    return true;
                }
                // Full helm replaces head + hair so the face does not poke through plate.
                let n = p.filename.to_ascii_lowercase();
                !(n.contains("head") || n.contains("hair"))
            })
            .map(|p| (EquipMesh::Base, p))
            .collect();
        if wear_helm {
            match resolver.part(race, gender, caer_assets::figures::PART_HELM) {
                Some(h) => parts.push((EquipMesh::Helm, h)),
                None => log::debug!(
                    "caer-render: MS-02b-full — helm equipped but no fig3 PART_HELM for race {race} gender {gender}"
                ),
            }
        }
        if wear_cloak {
            match resolver.part(race, gender, caer_assets::figures::PART_CLOAK) {
                Some(c) => parts.push((EquipMesh::Cloak, c)),
                None => log::debug!(
                    "caer-render: MS-02b-full — cloak equipped but no fig3 PART_CLOAK for race {race} gender {gender}"
                ),
            }
        }
        // Hair geometry belongs to `fig3map`'s PART_HAIR variants, not to the selected texture
        // sheet. The old filename-derived override silently rendered Briton Hair 1 for source
        // Hair 4 (its colour sheet is shared) and could never represent Bald. Replacing the
        // archive-qualified PartRef here means the source index survives all the way to the NIF
        // load below; there is no archive sweep or string heuristic left to drift it.
        if !wear_helm {
            match source_hair_mesh(resolver, race, gender, custom.hair_style) {
                HairMeshSelection::Base => {}
                HairMeshSelection::Source(selected) => {
                    for (kind, part) in &mut parts {
                        if matches!(*kind, EquipMesh::Base) && is_hair_part(part) {
                            *part = selected.clone();
                        }
                    }
                }
                HairMeshSelection::Omit => {
                    parts.retain(|(kind, part)| {
                        !matches!(*kind, EquipMesh::Base) || !is_hair_part(part)
                    });
                }
            }
        }
        if parts.is_empty() {
            log::debug!("caer-render: avatar — no fig3 parts for race {race} gender {gender}");
            return None;
        }
        let mut part_names = parts
            .iter()
            .map(|(_, p)| p.filename.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let root = terrain::client_root();
        // The avatar's idle clip comes from the anim tables keyed by race+gender ("Briton Male" →
        // set 2 → `I_hm.kfa`) — the same shared humanoid clip the creatures retarget (A.3.5). This
        // is what unblocked posing the avatar: there was no avatar-specific clip to find.
        let clip = self.resolve_avatar_clip(race, gender);
        // Load + parse each part NIF from its fig3 archive. A missing part is skipped (a body with
        // one absent piece still reads better than a box); a totally empty set fails.
        // Each part is read BOTH ways: rigged (to pose) and static (the fallback). A part that
        // carries no skin still renders in bind pose alongside its posed siblings.
        //
        // fig3 body parts carry NO bone hierarchy of their own — just a `"Scene Root"` node and a
        // `NiStringsExtraData` list of the skin's bone NAMES ("Bip01 Pelvis", "Bip01 L ThighTwist",
        // …). They bind to a shared EXTERNAL skeleton, which every race ships as a dedicated
        // `<token>_<m|f>_skeleton.nif` inside the `sfigNNN.mpk` archives.
        //
        // Verified by bind reconstruction vs the static mesh. Binding to each part's own 1-bone rig
        // was off by up to 66 units (the body collapsed); `figures/skeleton.nif` (54 bones) and the
        // `bcommonm`/`bcommonf` "common bodies" (114/113) both still missed by 66. The per-race
        // skeleton is the authored rig. The two fallbacks below cover any figure whose race token
        // doesn't resolve, and the bind check decides whether the result is usable either way.
        let member_bytes = |part: &caer_assets::figures::PartRef| -> Option<Vec<u8>> {
            caer_assets::open_member(root.join(part.archive_path()), &part.nif_member()).ok()?
        };
        // The sheet this character's hair wears, resolved once. It is material data only; the
        // geometry selection above remains driven by fig3map's exact hairstyle variant. Style
        // zero is the uncustomized default and has no style row, so it falls back to the
        // colour-row lookup just as the retail starting body does.
        let hair_skin_id: Option<u16> = self
            .fig3_look
            .as_ref()
            .zip(caer_assets::figures::FigureModels::race_name(race))
            .and_then(|(look, rname)| {
                look.hair_style_skin_id(rname, gender, custom.hair_style, custom.hair_color)
                    .or_else(|| look.hair_skin_id(rname, gender, custom.hair_color))
            });
        let mut missing_parts: Vec<String> = Vec::new();
        // CAER_NO_HAIR=1: drop hair parts. A direct experiment for "is the hair covering the
        // face", which no amount of asset measurement can answer.
        let drop_hair = std::env::var_os("CAER_NO_HAIR").is_some();
        let part_bytes: Vec<(EquipMesh, caer_assets::figures::PartRef, Vec<u8>)> = parts
            .iter()
            .filter(|(_, p)| !(drop_hair && is_hair_part(p)))
            .filter_map(|(kind, p)| match member_bytes(p) {
                Some(b) => Some((*kind, p.clone(), b)),
                None => {
                    missing_parts.push(p.filename.clone());
                    None
                }
            })
            .collect();
        if !missing_parts.is_empty() {
            part_names.push_str(" miss=");
            part_names.push_str(&missing_parts.join(","));
            log::debug!(
                "caer-render: avatar — race {race} gender {gender} missing NIF members: {}",
                missing_parts.join(",")
            );
        }
        // Primary source: the race's shared "common body" mesh, found through monnifs' Textual Name
        // ("Briton Male" → `bcommonm`, 114 bones). Only Briton Male's hair happens to ship a rig, so
        // scanning the parts alone leaves most races unposed; the table lookup covers them all.
        // Primary source: the richest skeleton among the figure's OWN parts. Measured, this is the
        // right one — Briton Male's hair ships the full 162-bone rig and every part binds to it
        // within 0.00–1.79 units. The `bcommonm`/`bcommonf` common bodies (114/113 bones) look like
        // the obvious answer but are NOT the authored rig: binding to them drifts by 66 units. They
        // stay only as a fallback for figures whose own parts ship no skeleton, where the bind
        // check below decides whether the result is usable.
        let shared_skeleton = self
            .race_skeleton(race, gender)
            .or_else(|| {
                part_bytes
                    .iter()
                    .filter_map(|(_, _, b)| caer_assets::nif::read_skeleton(b).ok())
                    .max_by_key(|s| s.bones.len())
                    .filter(|s| s.bones.len() > 1)
            })
            .or_else(|| self.common_body_skeleton(race, gender));

        let mut models = Vec::new();
        let mut rigs = Vec::new();
        // The source part for each entry in `rigs`, kept parallel so a batch part can be traced
        // back to the NIF it came from — that mapping is what names the head's texture, which the
        // mesh itself does not carry.
        let mut rig_parts: Vec<caer_assets::figures::PartRef> = Vec::new();
        let mut demoted = 0usize;
        let mut bind_errors: Vec<(String, f32)> = Vec::new();
        let stamp_equip_names =
            |kind: EquipMesh, filename: &str, rig: &mut caer_assets::nif::RiggedModel| {
                // Helm/cloak NIFs name shapes "Biped Object". Base-body parts often do too —
                // stamp a SkinSlot-recognisable name from the fig3 filename so pskins bind.
                let name = match kind {
                    EquipMesh::Helm => Some("Helm"),
                    EquipMesh::Cloak => Some("Cloak"),
                    EquipMesh::Base => SkinSlot::from_part_name(filename).map(|slot| match slot {
                        SkinSlot::Lbody => "Lbody",
                        SkinSlot::Body => "Body",
                        SkinSlot::Arms => "Arms",
                        SkinSlot::Gloves => "Gloves",
                        SkinSlot::Legs => "Legs",
                        SkinSlot::Boots => "Boots",
                        SkinSlot::Head => "HeadB",
                        SkinSlot::Face => "HeadA",
                        SkinSlot::Helm => "Helm",
                        SkinSlot::Cloak => "Cloak",
                    }),
                };
                if let Some(name) = name {
                    for p in &mut rig.parts {
                        if matches!(kind, EquipMesh::Helm | EquipMesh::Cloak)
                            || SkinSlot::from_part_name(&p.name).is_none()
                        {
                            p.name = name.to_string();
                        }
                    }
                }
            };
        for (kind, part, bytes) in &part_bytes {
            let stat = caer_assets::nif::read_model(bytes);
            if let (Some(skel), true) = (shared_skeleton.as_ref(), clip.is_some()) {
                if let Ok(Some(mut rig)) = caer_assets::nif::read_rigged_external(bytes, skel) {
                    stamp_equip_names(*kind, &part.filename, &mut rig);
                    // VALIDATE each part before trusting it: at bind pose its skinned vertices must
                    // reproduce its static mesh. A part that doesn't is bound to a different rig,
                    // and posing it would mangle that piece.
                    //
                    // The check is PER PART, not per body, because the two failure modes need
                    // different outcomes and this handles both: a wrong skeleton fails every part
                    // (so the whole body falls back to static, as a Troll on the human rig did),
                    // while an odd head/hair variant fails alone (3 of 36 race+genders — e.g.
                    // `sar_f_head01` at 14.6 with every other part at 0.0) and only that piece
                    // stays in bind pose instead of costing the body its animation.
                    // Log every part's bind error, not just failures: a part that PASSES the gate
                    // with an outlier value (say 4.9 where the rest are 0.0) is bound to something
                    // subtly wrong and will hinge about the wrong pivot when posed — visible as a
                    // joint that detaches. A pass/fail-only log hides exactly that case.
                    let err = stat
                        .as_ref()
                        .map(|s| bind_error(&rig, s))
                        .unwrap_or(f32::INFINITY);
                    log::debug!(
                        "caer-render: avatar — part {} bind error {err:.2} ({})",
                        part.nif_member(),
                        if err <= bind_gate() { "ok" } else { "DEMOTED" },
                    );
                    bind_errors.push((part.filename.clone(), err));
                    let ok = err <= bind_gate();
                    let lower = part.filename.to_ascii_lowercase();
                    let is_head = lower.contains("head");
                    // Measure the unmodified bind first — the static source model is the only
                    // valid reference for the skeleton gate — then apply the selected facial
                    // offsets in the same skin-space coordinates the GPU will later consume.
                    // This is intentionally after `bind_error`: a wider jaw is not a bad rig.
                    apply_facial_morphs(&mut rig, appearance);
                    // Hair is decoration: the body reads correctly without it, and it is the only
                    // base part that can fail alone on an otherwise perfectly bound skeleton.
                    let is_cosmetic = lower.contains("hair");
                    // Helm/cloak equipment meshes often fail the base-body bind gate but must stay on
                    // the GPU-skinned path — demoting them into `models` forces the CPU bake and drops
                    // all MS-02b pskin binds. Keep them skinned (or skip if unreadable).
                    // Troll Head is the same class of miss: demoting it into leftover models either
                    // drops the GPU path's head or forces the CPU bake that cannot bind cloth.
                    match kind {
                        EquipMesh::Helm | EquipMesh::Cloak => {
                            if !ok {
                                log::debug!(
                                    "caer-render: MS-02b-full — {} bind error {err:.2}; keeping on skinned path",
                                    part.nif_member()
                                );
                            }
                            rigs.push(rig);
                            rig_parts.push(part.clone());
                            continue;
                        }
                        EquipMesh::Base => {
                            if ok || is_head {
                                if !ok && is_head {
                                    part_names.push_str(" head_bind=");
                                    part_names.push_str(&format!("{err:.2}"));
                                    log::debug!(
                                        "caer-render: avatar — Head {} bind error {err:.2}; keeping on skinned path",
                                        part.nif_member()
                                    );
                                }
                                rigs.push(rig);
                                rig_parts.push(part.clone());
                                continue;
                            }
                            // A COSMETIC part that fails the gate is dropped, not demoted.
                            //
                            // Demoting pushes it into `models`, and a non-empty `models` sends the
                            // whole body down the CPU-baked path — no skeleton, no animation, no
                            // textures. Lurikeen female measured 0.0 bind error on every body part
                            // and 10.0 on `lur_f_hair01` alone: she was losing her entire body,
                            // pose and clothing to one bad hairstyle. Losing the hairstyle is the
                            // cheaper failure by a wide margin.
                            //
                            // Deliberately NOT extended to body parts. Firbolg fails at 49-68
                            // across body/arms/gloves/head, which is a skeleton mismatch, and
                            // dropping those would render a torso-less character while claiming
                            // success. That one has to stay loud.
                            if is_cosmetic {
                                part_names.push_str(" dropped=");
                                part_names.push_str(&part.filename);
                                log::debug!(
                                    "caer-render: avatar — cosmetic {} failed the bind gate at \
                                     {err:.2}; dropping it rather than costing the body its rig",
                                    part.nif_member()
                                );
                                continue;
                            }
                            demoted += 1;
                            part_names.push_str(" demoted=");
                            part_names.push_str(&part.filename);
                        }
                    }
                }
            }
            // Base-body demotion → static leftover. Never push failed helm/cloak here (see above).
            if matches!(kind, EquipMesh::Base) {
                match stat {
                    Ok(mut model) => {
                        apply_facial_morphs_to_model(&mut model, appearance);
                        models.push(model);
                    }
                    Err(e) => log::debug!(
                        "caer-render: avatar — {} parse failed: {e}",
                        part.nif_member()
                    ),
                }
            } else {
                log::debug!(
                    "caer-render: MS-02b-full — skipping unequippable {} (no external skin)",
                    part.nif_member()
                );
            }
        }
        if demoted > 0 {
            log::debug!("caer-render: avatar — race {race} gender {gender}: {demoted} part(s) failed the bind check, held in bind pose");
        }
        // GPU-skinned path when every usable part rigged against the shared skeleton: upload the
        // body once in skin space and animate it per frame, exactly like a creature. Falls back to
        // the CPU-baked assembly when there's no clip or no rig (nothing regresses).
        if let (Some(clip), false) = (&clip, rigs.is_empty()) {
            // CAER_POSE_REPORT=1: per-bone displacement between bind and this posed frame.
            //
            // `CAER_NO_POSE` answers "is it the pose?" but also switches to the static CPU
            // path, so it cannot isolate posing on its own. This keeps the path identical and
            // names WHICH bone moves, which is what distinguishes a stretched mesh from a
            // rotated one.
            if std::env::var_os("CAER_POSE_REPORT").is_some() {
                if let Some(rig) = rigs.first() {
                    let t = pose_report_t(clip.duration);
                    let posed = rig.skeleton.pose(clip, t);
                    let mut rows: Vec<(f32, String)> = rig
                        .skeleton
                        .bones
                        .iter()
                        .enumerate()
                        .filter_map(|(i, bone)| {
                            let p = caer_assets::nif::xform_to_mat4(posed.get(i)?);
                            let w = caer_assets::nif::xform_to_mat4(&bone.world_bind);
                            let d = ((p[3][0] - w[3][0]).powi(2)
                                + (p[3][1] - w[3][1]).powi(2)
                                + (p[3][2] - w[3][2]).powi(2))
                            .sqrt();
                            Some((d, format!("{i:>4} {}", bone.name)))
                        })
                        .collect();
                    rows.sort_by(|a, b| b.0.total_cmp(&a.0));
                    println!(
                        "rustdaoc: pose-report race {race} gender {gender} t={t:.2} clip={:.2}s rate={:.3} tracks={} bones={}",
                        clip.duration,
                        clip.rate,
                        clip.tracks.len(),
                        rig.skeleton.bones.len()
                    );
                    for (d, name) in rows.iter().take(6) {
                        println!("  bind->posed {d:8.2}  {name}");
                    }
                    // Orientation of the head/neck/spine chain, in BIND and POSED, as the elevation
                    // of each local axis above the horizon.
                    //
                    // Displacement (above) cannot see a tilt: a head that pitches about its own
                    // joint barely moves its origin, and the per-part AABB below is axis-aligned, so
                    // it rounds a rotation away too. Both are why "the female faces point up and the
                    // males are fine" was not visible in this report before.
                    //
                    // Bind *and* posed on purpose. If the two genders differ in POSED but agree in
                    // BIND, the clip or the retarget is tilting the head; if they differ in BIND, the
                    // mesh or its rig is, and no amount of animation work will fix it.
                    let elevation = |m: &[[f32; 4]; 4], axis: usize| {
                        let v = [m[axis][0], m[axis][1], m[axis][2]];
                        let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                        if len < 1e-6 {
                            return 0.0;
                        }
                        (v[2] / len).clamp(-1.0, 1.0).asin().to_degrees()
                    };
                    for (i, bone) in rig.skeleton.bones.iter().enumerate() {
                        let n = bone.name.to_ascii_lowercase();
                        if !(n.contains("head") || n.contains("neck")) || n.contains("nub") {
                            continue;
                        }
                        let Some(px) = posed.get(i) else { continue };
                        let pm = caer_assets::nif::xform_to_mat4(px);
                        let bm = caer_assets::nif::xform_to_mat4(&bone.world_bind);
                        // Whether the CLIP keys this bone at all is the discriminator that splits
                        // "the animation authors this head pose" from "we are inventing it". A bone
                        // with no track inherits its parent rigidly; one with a track is being told
                        // where to point.
                        // `bone.id`, NOT the loop index. `Skeleton::pose_into` looks a track up as
                        // `b.id.and_then(|id| clip.tracks.get(&id))`, and a first version of this
                        // line used `i` — which reported every bone unkeyed and had me one step from
                        // filing a retargeting bug that did not exist. An instrument that indexes
                        // differently from the code it observes is measuring a different program.
                        let keyed = bone.id.is_some_and(|id| clip.tracks.contains_key(&id));
                        println!(
                            "  orient {:<22} keyed={keyed:<5} bind X{:>7.2} Y{:>7.2} Z{:>7.2}  posed X{:>7.2} Y{:>7.2} Z{:>7.2}  (deg above horizon)",
                            bone.name,
                            elevation(&bm, 0),
                            elevation(&bm, 1),
                            elevation(&bm, 2),
                            elevation(&pm, 0),
                            elevation(&pm, 1),
                            elevation(&pm, 2),
                        );
                    }
                    // Per-part bind vs posed extents. Bind-error is blind to weighting: at bind
                    // every bone sits at its bind transform, so weights cannot show up there. A
                    // part whose POSED bounding box balloons is being stretched by the pose.
                    for r in &rigs {
                        let pal = r.skinning_palette(clip, t);
                        for (pi, part) in r.parts.iter().enumerate() {
                            let (mut b0, mut b1) = ([f32::MAX; 3], [f32::MIN; 3]);
                            let (mut p0, mut p1) = ([f32::MAX; 3], [f32::MIN; 3]);
                            for vi in 0..part.positions.len() {
                                let b = r.bind_position(pi, vi);
                                let a = r.animated_position(&pal, pi, vi);
                                for k in 0..3 {
                                    b0[k] = b0[k].min(b[k]);
                                    b1[k] = b1[k].max(b[k]);
                                    p0[k] = p0[k].min(a[k]);
                                    p1[k] = p1[k].max(a[k]);
                                }
                            }
                            if part.positions.is_empty() {
                                continue;
                            }
                            let be = [b1[0] - b0[0], b1[1] - b0[1], b1[2] - b0[2]];
                            let pe = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
                            let grow = (pe[0] * pe[1] * pe[2]) / (be[0] * be[1] * be[2]).max(0.001);
                            println!(
                                "  part {:<10} bind {:.0}x{:.0}x{:.0} -> posed {:.0}x{:.0}x{:.0}  vol x{grow:.2}",
                                part.name, be[0], be[1], be[2], pe[0], pe[1], pe[2]
                            );
                            // Weight health. Bind-error cannot see this: at bind every bone is at
                            // its bind transform, so any weighting reproduces the mesh. Under a
                            // pose, vertices whose weights do not sum to 1 are under-transformed
                            // and trail behind — which reads as stretching.
                            let mut low = 0usize;
                            let mut worst = 1.0f32;
                            let mut zero = 0usize;
                            for w in &part.weights {
                                let sum: f32 = w.iter().sum();
                                if sum < 0.99 {
                                    low += 1;
                                }
                                if sum <= 0.001 {
                                    zero += 1;
                                }
                                worst = worst.min(sum);
                            }
                            println!(
                                "       weights: {low}/{} verts sum<0.99, {zero} with none, min sum {worst:.3}",
                                part.weights.len()
                            );
                            // Which bones actually drive this part, and how far each moves under
                            // the pose. A part weighted to a chain that swings apart stretches.
                            if part.name.to_ascii_lowercase().contains("glove") {
                                let mut used: std::collections::BTreeMap<u16, usize> =
                                    Default::default();
                                for (js, ws) in part.joints.iter().zip(part.weights.iter()) {
                                    for (j, w) in js.iter().zip(ws.iter()) {
                                        if *w > 0.01 {
                                            *used.entry(*j).or_insert(0) += 1;
                                        }
                                    }
                                }
                                let mut rows: Vec<(f32, String, usize)> = used
                                    .iter()
                                    .filter_map(|(j, n)| {
                                        let b = r.skeleton.bones.get(*j as usize)?;
                                        let pj = caer_assets::nif::xform_to_mat4(
                                            posed.get(*j as usize)?,
                                        );
                                        let wb = caer_assets::nif::xform_to_mat4(&b.world_bind);
                                        let d = ((pj[3][0] - wb[3][0]).powi(2)
                                            + (pj[3][1] - wb[3][1]).powi(2)
                                            + (pj[3][2] - wb[3][2]).powi(2))
                                        .sqrt();
                                        Some((d, b.name.clone(), *n))
                                    })
                                    .collect();
                                rows.sort_by(|a, b| b.0.total_cmp(&a.0));
                                println!("       {} driving bones (top by movement):", rows.len());
                                for (d, n, c) in rows.iter().take(6) {
                                    println!("         moved {d:7.2}  {c:>4} verts  {n}");
                                }
                                // Retarget check. `sample_local` keeps this skeleton's BIND
                                // translation for every non-root bone and takes the clip's only for
                                // the root chain, so bone lengths are the target rig's and a clip
                                // authored for different proportions cannot stretch a limb. What
                                // this measures is how far the two disagree, which may indicate a
                                // clip authored for materially different proportions. The policy
                                // exempts the root-motion carriers — the root and its direct
                                // children — not simply non-root bones. A gap is a lead, not a
                                // verdict.
                                let mut worst_t = 0.0f32;
                                let mut n_t = 0usize;
                                let mut example = String::new();
                                for j in used.keys() {
                                    let Some(b) = r.skeleton.bones.get(*j as usize) else {
                                        continue;
                                    };
                                    let Some(id) = b.id else { continue };
                                    let Some(tr) = clip.tracks.get(&id) else {
                                        continue;
                                    };
                                    if tr.translation.is_empty() {
                                        continue;
                                    }
                                    n_t += 1;
                                    let k = tr.translation[tr.translation.len() / 2].value;
                                    let bl = b.local.2;
                                    let d = ((k[0] - bl[0]).powi(2)
                                        + (k[1] - bl[1]).powi(2)
                                        + (k[2] - bl[2]).powi(2))
                                    .sqrt();
                                    if d > worst_t {
                                        worst_t = d;
                                        example = format!(
                                            "{} clip({:.1},{:.1},{:.1}) vs bind({:.1},{:.1},{:.1})",
                                            b.name, k[0], k[1], k[2], bl[0], bl[1], bl[2]
                                        );
                                    }
                                }
                                println!("       retarget: {n_t}/{} driving bones carry clip TRANSLATION keys; worst |clip-bind| {worst_t:.2}",
                                    used.len());
                                if !example.is_empty() {
                                    println!("         {example}");
                                }
                            }
                        }
                    }
                }
            }
            let batch = terrain::skinned_batch_multi(&rigs);
            if !batch.indices.is_empty() {
                // Foot-anchor via the palette rather than the vertices: skinned vertices are in
                // skin space, so the shift has to ride on the bone transforms (see
                // SkinnedRig::z_offset). Bounds here are bind-pose, which is what we want.
                let z_offset = -batch.bound_min[2];
                self.heights
                    .insert(id, batch.bound_max[2] - batch.bound_min[2]);
                log::debug!(
                    "caer-render: avatar — race {race} gender {gender}: GPU-skinned, {} of {} parts, {} verts, height {:.0}, foot offset {z_offset:.0}",
                    rigs.len(), parts.len(), batch.vertices.len(), batch.bound_max[2] - batch.bound_min[2],
                );
                if !models.is_empty() {
                    // A part that carried no skin can't join a skinned draw; dropping it loses a
                    // piece of the body, so prefer the CPU path in that case rather than render a
                    // headless avatar.
                    log::debug!("caer-render: avatar — {} unrigged part(s); using the CPU-baked path instead", models.len());
                } else {
                    // Name each part's texture, then load them. A part whose NIF names a texture
                    // (hair) uses that; one that doesn't (head, body) falls back to `<part>.dds`,
                    // which is how the per-race head texture is keyed in Mskins.
                    // The head's texture is keyed by mesh name (`hig_f_head01.nif` ->
                    // `hig_f_head01.dds`) and binds directly.
                    //
                    // An earlier pass left heads white on the belief that this DDS is a face ATLAS
                    // whose sub-region must be chosen through `fig3facemap.csv`. MEASURED against
                    // the art, that is false: `bri_m_head01`, `bri_f_head01`, `hig_m_head01` and
                    // `tro_m_head01` are each a uniformly dense 256x256 sheet, 100% opaque, with no
                    // banding or empty gutters — one face per file, not many. `fig3facemap` selects
                    // WHICH file a face type uses, not a rectangle inside one.
                    //
                    // See `head_texture_is_an_atlas_or_a_single_face` in tests/preworld_avatars.rs.
                    let mut batch = batch;
                    let mut part_textures: HashMap<String, caer_assets::dds::DdsTexture> =
                        HashMap::new();
                    // Face and hair both come from look tables keyed by race/gender, and neither
                    // is named after its mesh: the hair mesh is `Hig_M_hair01` while its texture
                    // is `Hig_m_Hair1_blonde.dds`. Binding `<mesh>.dds` therefore leaves hair
                    // white on every race — which reads as a bald white skull-cap over a correctly
                    // textured face, i.e. "no face, eyes and mouth floating".
                    // Resolve the look-table skin ids first, then load — the id lookup borrows
                    // `self.resolver` and the Mskins fallback needs `self.mskin_index` mutably.
                    let mut look_skins: Vec<(&str, caer_assets::monsters::SkinRef)> = Vec::new();
                    if let Some(look) = self.fig3_look.as_ref() {
                        if let Some(rname) = caer_assets::figures::FigureModels::race_name(race) {
                            for (slot, sid) in [
                                ("head", look.face_skin_id(rname, gender, custom.face_type)),
                                ("hair", hair_skin_id),
                            ] {
                                match sid.and_then(|s| self.resolver.skin(s)) {
                                    Some(skin) => look_skins.push((slot, skin.clone())),
                                    None => log::debug!(
                                        "caer-render: avatar — race {race} gender {gender} has no \
                                         {slot} skin in the look tables"
                                    ),
                                }
                            }
                        }
                    }
                    for (slot, skin) in look_skins {
                        // Faces live in `figures/skins/`; hair lives in `figures/Mskins/`. Trying
                        // only the first is why every race rendered a white skull-cap over a
                        // correctly textured face.
                        let tex = load_skin(&skin)
                            .or_else(|| avatar_texture(&mut self.mskin_index, &skin.dds));
                        let Some(tex) = tex else {
                            log::debug!(
                                "caer-render: avatar — race {race} {slot} skin {} not found in \
                                 figures/skins or figures/Mskins",
                                skin.dds
                            );
                            continue;
                        };
                        // A FACE is never see-through, so force its alpha opaque exactly as
                        // clothing already is. These sheets carry real DXT3/DXT5 alpha averaging
                        // ~0.2, and `skinned.wgsl` discards every texel under 0.4 — which threw
                        // away 85-90% of every face and left only the high-alpha eye and teeth
                        // islands. That is the "floating eyes and mouth on a blank head" report.
                        // Briton escaped it by resolving to a DXT1 copy, which carries no alpha
                        // channel at all. HAIR keeps its cutout: it genuinely needs the gaps.
                        let (key, tex) = if slot == "head" {
                            let k = format!("{}#opaque", skin.dds);
                            (k, force_opaque(&tex).unwrap_or(tex))
                        } else {
                            (skin.dds.clone(), tex)
                        };
                        // `HeadB` is the HAIR slot, not the face. A plain `contains("head")` gave
                        // it the face sheet, so Briton's beard drew in skin tone and it never
                        // matched the "hair" pass at all. Kobold escaped only because its hair
                        // part kept its mesh name (`kob_m_hair01`).
                        let takes = |name: &str| {
                            let n = name.to_ascii_lowercase();
                            match slot {
                                "head" => n.contains("head") && !n.contains("headb"),
                                "hair" => n.contains("hair") || n.contains("headb"),
                                _ => n.contains(slot),
                            }
                        };
                        for bp in &mut batch.parts {
                            if takes(&bp.name) {
                                bp.texture = Some(key.clone());
                            }
                        }
                        part_textures.insert(key, tex);
                    }
                    for bp in &mut batch.parts {
                        if bp.texture.is_none() {
                            let n = bp.name.to_ascii_lowercase();
                            if n.contains("head") {
                                bp.texture = Some(format!("{n}.dds"));
                            }
                        }
                    }
                    for bp in &batch.parts {
                        let Some(name) = bp.texture.as_deref() else {
                            continue;
                        };
                        if part_textures.contains_key(name) {
                            continue;
                        }
                        match avatar_texture(&mut self.mskin_index, name) {
                            Some(t) => {
                                part_textures.insert(name.to_string(), t);
                            }
                            None => log::debug!("caer-render: avatar — no texture for {name}"),
                        }
                    }
                    let female = gender == caer_assets::figures::GENDER_FEMALE;
                    let mskin_bound =
                        self.assign_mskin_cloth(race, female, &mut batch, &mut part_textures);
                    // MS-02b: equipment object models → pskins overlay on Body/Arms/… parts.
                    let eq_bound = if let Some(eq) = equip {
                        self.assign_equipment_skins(eq, &mut batch, &mut part_textures, female)
                    } else {
                        0
                    };
                    if let Some(look) = self.fig3_look.as_ref() {
                        if let Some(rname) = caer_assets::figures::FigureModels::race_name(race) {
                            if let Some(rgb) = look.skin_rgb(rname, gender, custom.eye_color) {
                                tint_untextured(&mut batch, &part_textures, rgb);
                            }
                        }
                    }
                    let textured = batch
                        .parts
                        .iter()
                        .filter(|p| {
                            p.texture
                                .as_deref()
                                .is_some_and(|n| part_textures.contains_key(n))
                        })
                        .count();
                    if std::env::var_os("CAER_POSE_REPORT").is_some() {
                        println!("rustdaoc: bound-textures race {race} gender {gender}");
                        for bp in &batch.parts {
                            let t = bp.texture.as_deref().unwrap_or("<none>");
                            println!(
                                "  {:<16} <- {t}  loaded={}",
                                bp.name,
                                part_textures.contains_key(t)
                            );
                        }
                    }
                    self.record_stand(
                        id,
                        AvatarStandInfo {
                            fig3_parts: parts.len(),
                            textured,
                            eq_bound,
                            mskin_bound,
                            path: "gpu",
                            part_names: part_names.clone(),
                            bind_errors: bind_errors.clone(),
                            bound_textures: batch
                                .parts
                                .iter()
                                .map(|p| {
                                    (
                                        p.name.clone(),
                                        p.texture.clone().unwrap_or_else(|| "<none>".into()),
                                    )
                                })
                                .collect(),
                        },
                    );
                    log::debug!(
                        "caer-render: avatar — {textured} of {} parts textured, {eq_bound} equip-bound, {mskin_bound} mskin (skin={skin_key:?})",
                        batch.parts.len(),
                    );
                    // `CAER_PART_TINT` floods each part's sheet with one flat colour, so a
                    // capture becomes a part-id map: whatever `caer audit filler` finds can be
                    // attributed to the part that drew it instead of matched by eye. The legend
                    // goes to stdout because the colours mean nothing without it.
                    if std::env::var_os("CAER_PART_TINT").is_some() {
                        println!("rustdaoc: part-tint legend race {race} gender {gender}");
                        let mut tinted: HashMap<String, String> = HashMap::new();
                        for (i, bp) in batch.parts.iter().enumerate() {
                            let Some(name) = bp.texture.as_deref() else {
                                continue;
                            };
                            let rgb = crate::uv_coverage::part_tint_colour(i);
                            println!(
                                "  {:<20} <- rgb({:>3},{:>3},{:>3})  {name}",
                                bp.name, rgb[0], rgb[1], rgb[2]
                            );
                            // One part per colour: a sheet shared by two parts would otherwise
                            // take whichever tint was written last and mislabel both.
                            let key = format!("{name}#tint{i}");
                            if let Some(t) = part_textures.get(name) {
                                if let Some(flat) = flood_colour(t, rgb) {
                                    tinted.insert(bp.name.clone(), key.clone());
                                    part_textures.insert(key, flat);
                                }
                            }
                        }
                        for bp in batch.parts.iter_mut() {
                            if let Some(key) = tinted.get(&bp.name) {
                                bp.texture = Some(key.clone());
                            }
                        }
                    }
                    // `CAER_FILLER_PROBE` repaints the space between a sheet's islands magenta.
                    // Any magenta in the resulting frame is a triangle sampling that space where
                    // the player can see it — the screen-space form of the "unrendered triangles"
                    // reports, with no threshold to argue about.
                    if std::env::var_os("CAER_FILLER_PROBE").is_some() {
                        for tex in part_textures.values_mut() {
                            if let Some(painted) = paint_filler(tex) {
                                *tex = painted;
                            }
                        }
                    }
                    self.last_avatar_textures = part_textures.clone();
                    if gpu.upload_skinned_mesh(id, &batch, None, &part_textures, z_offset) {
                        self.remember_resident_mesh(id, base_id, tiers, skin_key, appearance);
                        let loco = self.avatar_locomotion_clips(race, gender);
                        log::debug!(
                            "caer-render: avatar — race {race} gender {gender} loco clips: walk {} run {} back {} slideL {} slideR {}",
                            loco.walk.is_some(), loco.run.is_some(), loco.back.is_some(),
                            loco.slide_left.is_some(), loco.slide_right.is_some(),
                        );
                        self.rigs.insert(
                            id,
                            SkinnedRig {
                                rigs,
                                clip: clip.clone(),
                                walk: loco.walk,
                                run: loco.run,
                                back: loco.back,
                                slide_left: loco.slide_left,
                                slide_right: loco.slide_right,
                                // The avatar's strides aren't in monnifs (it isn't a creature row); its
                                // clips play at their authored rate until the player-movement slice
                                // measures a real stride.
                                stride_walk: 0.0,
                                stride_run: 0.0,
                                stride_back: 0.0,
                                stride_strafe: 0.0,
                                z_offset,
                            },
                        );
                        return Some(id);
                    }
                }
            }
        }

        // CPU-baked fallback: pose the rigged parts against the shared clip, then append leftovers.
        let mut batch = match (&clip, rigs.is_empty()) {
            (Some(clip), false) => {
                let t = POSE_FRAME_T.min(clip.duration);
                log::debug!(
                    "caer-render: avatar — {} of {} parts posed from idle clip ({:.2}s @ t={t:.2})",
                    rigs.len(),
                    parts.len(),
                    clip.duration
                );
                let mut b = terrain::posed_batch_multi(&rigs, clip, t);
                terrain::append_models(&mut b, &models);
                b
            }
            _ => terrain::untextured_batch_multi(&models),
        };
        if batch.indices.is_empty() {
            log::debug!("caer-render: avatar — assembled mesh empty (race {race} gender {gender})");
            return None;
        }
        // Foot-anchor the body: fig3 parts are authored with the origin above the feet, so shift the
        // whole mesh down by its lowest vertex (feet in the standing bind pose). Then the renderer's
        // foot-anchored grounding (+0) seats the character ON the terrain instead of hovering.
        let foot = batch.bound_min[2];
        self.heights
            .insert(id, batch.bound_max[2] - batch.bound_min[2]);
        if foot.abs() > f32::EPSILON {
            for v in &mut batch.vertices {
                v.pos[2] -= foot;
            }
            batch.bound_min[2] -= foot;
            batch.bound_max[2] -= foot;
        }
        log::debug!(
            "caer-render: avatar — race {race} gender {gender}: {} parts, {} verts, height {:.0}",
            models.len(),
            batch.vertices.len(),
            batch.bound_max[2]
        );
        self.record_stand(
            id,
            AvatarStandInfo {
                fig3_parts: parts.len(),
                textured: 0,
                eq_bound: 0,
                mskin_bound: 0,
                path: "cpu",
                part_names,
                bind_errors: bind_errors.clone(),
                bound_textures: Vec::new(),
            },
        );
        // CPU-baked path has no per-part texture slots (single mesh upload). Equipment textures
        // bind on the GPU-skinned path above; naked/fallback stays white here.
        gpu.upload_entity_mesh(id, &batch, None);
        self.remember_resident_mesh(id, base_id, tiers, skin_key, appearance);
        Some(id)
    }

    /// Give every part of a creature mesh the skin its BODY SLOT names, and load those skins.
    ///
    /// A creature is not one texture. Model 82 (`Saracen Female Merchant 2`) names seven different
    /// skins across its 49 parts, and its head skin (1043) is unrelated to its torso skin (122);
    /// binding the torso to the whole mesh drew that texture across the face, which is why every
    /// humanoid NPC looked melted. The parts are named by slot in the NIF (`Body1`, `HeadA2`,
    /// `Boots1`), which is the only thing connecting mesh geometry to those columns.
    ///
    /// Returns the map `upload_skinned_mesh` keys on, and stamps each part's texture key to match.
    /// Parts that already carry a NIF-named texture keep it — the table is authoritative only where
    /// the mesh itself says nothing, which is the case for every creature but NOT for the avatar's
    /// hair. Parts naming no known slot simply keep `None` and fall back to the body skin, so this
    /// can only add correctness to a creature that previously drew on a single skin.
    fn assign_part_skins(
        &self,
        model_id: u16,
        batch: &mut terrain::SkinnedBatch,
    ) -> HashMap<String, caer_assets::dds::DdsTexture> {
        let mut out: HashMap<String, caer_assets::dds::DdsTexture> = HashMap::new();
        // Slots whose DDS could not be loaded, so one miss doesn't retry per part.
        let mut missing: std::collections::HashSet<String> = std::collections::HashSet::new();
        for part in &mut batch.parts {
            if part.texture.is_some() {
                continue;
            }
            let Some(slot) = SkinSlot::from_part_name(&part.name) else {
                continue;
            };
            let Some(skin) = self.resolver.part_skin(model_id, slot) else {
                continue;
            };
            // Key on the resolved DDS, not the slot: slots that share a skin then share one upload.
            let key = format!("{}#{}", skin.archive, skin.dds);
            if missing.contains(&key) {
                continue;
            }
            if !out.contains_key(&key) {
                match load_skin(skin) {
                    Some(t) => {
                        out.insert(key.clone(), t);
                    }
                    None => {
                        log::debug!(
                            "caer-render: model {model_id} — no skin image for {slot:?} ({key})"
                        );
                        missing.insert(key);
                        continue;
                    }
                }
            }
            part.texture = Some(key);
        }
        out
    }

    fn record_stand(&mut self, id: u16, info: AvatarStandInfo) {
        self.last_stand = info.clone();
        self.last_stands.insert(id, info);
    }

    /// The sheet as the GPU received it, keyed exactly as the batch references it.
    ///
    /// Deliberately separate from [`avatar_sheet`], which re-decodes the archive. The two answer
    /// different questions and a diagnostic needs both: authored alpha says whether a sheet is an
    /// overlay, and the uploaded RGB says what the screen samples. Collapsing them onto the upload
    /// makes every sheet read opaque and the overlay census silently reads zero.
    #[must_use]
    pub fn avatar_sheet_uploaded(&self, key: &str) -> Option<&caer_assets::dds::DdsTexture> {
        self.last_avatar_textures.get(key)
    }

    /// Decode a bound avatar sheet by its texture key, through the same index the binder uses.
    ///
    /// Keys may carry the `#opaque` suffix `assign_mskin_cloth` adds; that names a cached variant,
    /// not a different file, so it is stripped here.
    ///
    /// Exposed so a diagnostic reads the sheet the renderer actually bound rather than building a
    /// second index with its own search order. That matters: the client ships
    /// `cthLegs01_ss_Hig_M.dds` twice — DXT1 in `mskin014` and DXT5 in `mskin038` — and which one
    /// is in play depends entirely on the index's archive ordering.
    pub fn avatar_sheet(&mut self, key: &str) -> Option<caer_assets::dds::DdsTexture> {
        let name = key.split('#').next().unwrap_or(key);
        // **Order matters and it is not arbitrary.** The binder resolves a look-table skin as
        // `load_skin(..).or_else(|| avatar_texture(..))` — `figures/skins` first, `figures/Mskins`
        // second — and several faces exist in both roots under the same name
        // (`bri_f_head01.dds` is in `mskin001`, `skin149` and `skin151`). Searching Mskins first
        // reads a different file than the renderer bound, which is the exact failure this method
        // was written to prevent.
        let idx = self.skin_index.get_or_insert_with(|| {
            let mut map = HashMap::new();
            let dir = terrain::client_root().join("figures/skins");
            if let Ok(rd) = std::fs::read_dir(&dir) {
                let mut archives: Vec<_> = rd.flatten().map(|e| e.path()).collect();
                archives.sort();
                for arc in archives {
                    let Ok(names) = caer_assets::list_names(&arc) else {
                        continue;
                    };
                    for n in names {
                        map.entry(n.to_ascii_lowercase())
                            .or_insert_with(|| arc.clone());
                    }
                }
            }
            map
        });
        if let Some(arc) = idx.get(&name.to_ascii_lowercase()) {
            if let Ok(Some(dds)) = caer_assets::open_member(arc, name) {
                if let Ok(t) = caer_assets::dds::read_model_dds(&dds) {
                    return Some(t);
                }
            }
        }
        avatar_texture(&mut self.mskin_index, name)
    }

    pub fn last_avatar_stand(&self) -> &AvatarStandInfo {
        &self.last_stand
    }

    /// `monsters.csv` Scale for this race's living model — fig3 synthetic ids are not in that table.
    #[must_use]
    pub fn race_display_scale(&self, race: u8, fig3_gender: u8) -> f32 {
        let wire = fig3_gender.saturating_sub(1);
        caer_protocol::career::race_model(race, wire)
            .map(|mid| self.resolver.model_scale(mid))
            .unwrap_or(1.0)
    }

    fn assign_mskin_cloth(
        &mut self,
        race: u8,
        female: bool,
        batch: &mut terrain::SkinnedBatch,
        part_textures: &mut HashMap<String, caer_assets::dds::DdsTexture>,
    ) -> usize {
        let mut bound = 0usize;
        for part in &mut batch.parts {
            if part
                .texture
                .as_deref()
                .is_some_and(|n| part_textures.contains_key(n))
            {
                continue;
            }
            let Some(slot) = SkinSlot::from_part_name(&part.name) else {
                continue;
            };
            if matches!(
                slot,
                SkinSlot::Face | SkinSlot::Head | SkinSlot::Helm | SkinSlot::Cloak
            ) {
                continue;
            }
            // The race's STARTING SET, named the way `gamedata.mpk : mskins.csv` names it.
            //
            // The fig3 base-body meshes carry no texture reference at all — 257 of the 262 parts
            // across every legal race and gender — so their skin comes from a table. The client
            // ships one per race: `cth<Slot>01_SS_<Token>[_M|_F].dds`, where `SS` is the starting
            // set and `<Token>` is the same short race token the meshes and skeletons use. That is
            // why a Highlander wears a tartan kilt (`cthLegs01_ss_Hig_M`) and a Briton does not.
            //
            // Two earlier versions of this were invented rather than read: `<slot>01_<Realm>_
            // <cloth|leather|skeleton>.dds` matched nothing but three Albion *skeleton* textures,
            // and a flat `cth<Slot>01_01.dds` dressed every race in the same generic cloth. Both
            // scored full marks on a binder that only counted whether *something* bound.
            let slot_name = match slot {
                SkinSlot::Body => "cthBody01",
                SkinSlot::Lbody => "sclBody01",
                SkinSlot::Arms => "cthArms01",
                SkinSlot::Gloves => "cthGloves01",
                SkinSlot::Legs => "cthLegs01",
                SkinSlot::Boots => "cthBoots01",
                _ => continue,
            };
            // MEASURED: `cthArms01_SS_*` and `cthGloves01_SS_*` carry alpha where the BARE
            // forearm and hand sample them — the art is authored expecting skin to show through.
            // With no skin layer beneath, those parts render as holes, which is the "Briton has no
            // arms" report. Swapping them onto the body atlas makes the limbs reappear (verified
            // visually), so the geometry and the bind are fine and the missing piece is the SKIN
            // itself: `fig3skincolormap` -> `fig3tints` resolves a real per-race tone that nothing
            // currently applies to a draw. Bare limbs are tinted skin, not textured cloth.
            //
            // Left binding the correct-by-name texture rather than a wrong-by-UV substitute; the
            // fix is the tint channel, not a different atlas.
            let g = if female { 'F' } else { 'M' };
            let mut names = Vec::new();
            // The Highlander's kilt is the only LBody a base body carries, and the client gives it
            // one texture by name rather than a per-race starting-set entry.
            if slot == SkinSlot::Lbody {
                names.push("cthLbody01_kilt01.dds".to_string());
            }
            // Case varies row to row in the table (`SS`/`ss`, `Bri`/`bri`, `_F`/`_f`); the index
            // this resolves against is lowercased, so only the spelling has to be right.
            if let Some(tok) = caer_assets::figures::FigureModels::race_token(race) {
                names.push(format!("{slot_name}_SS_{tok}_{g}.dds"));
                names.push(format!("{slot_name}_SS_{tok}.dds"));
            }
            // Eden's Highlander male starter outfit uses the unisex leg sheet.  The gendered
            // `_M` file is a different, paler bare-leg variant that happens to be present in the
            // retail tree.  Keep the unisex candidate first for this one race/slot and do not let
            // the mskins index filter it away when the CSV only indexes the gendered duplicate.
            let highlander_male_legs = race == 3 && !female && slot == SkinSlot::Legs;
            if highlander_male_legs {
                if let Some(i) = names
                    .iter()
                    .position(|name| name.eq_ignore_ascii_case("cthLegs01_SS_Hig.dds"))
                {
                    names.swap(0, i);
                }
            }
            // **Prefer what the client indexes over what happens to be on disk.** `mskins.csv` lists
            // every skin the client ships and the archive it lives in; the constructed names above
            // are a guess that this loop then confirms by *finding a file*. Those are different
            // questions, and 15 bindings answer them differently — every one male, including Briton
            // male legs, which is half of the pair in Matt's side-by-side. `cthLegs01_ss_Bri_m.dds`
            // sits in `mskin014` and is **not in the table**: the client dresses Briton male legs in
            // the unisex `cthLegs01_ss_Bri.dds`. Preferring the gendered spelling bound a leftover.
            //
            // Retained rather than filtered when the table is missing, because a client tree we
            // cannot read should degrade to the old behaviour rather than to no cloth at all.
            if !highlander_male_legs {
                if let Some(tbl) = self.mskins.as_ref() {
                    if names.iter().any(|n| tbl.indexed(n)) {
                        names.retain(|n| tbl.indexed(n));
                    }
                }
            }
            // **A fully opaque sheet is a GARMENT; a translucent one is bare-limb art.**
            //
            // The client indexes both spellings for several race+slot pairs, and taking the
            // gendered one first put Briton female in `cthLegs01_SS_bri_F` — a painted bare leg at
            // alpha 62 — instead of `cthLegs01_SS_Bri`, the opaque brown leggings Eden shows her
            // wearing. That is the "Briton female has no pants" report: not a missing layer, the
            // wrong sheet. The starting set dresses this slot, so when the table offers both, the
            // one that is cloth all the way through is the one being worn.
            //
            // Measured across all 18 races: this changes exactly one binding (Briton female legs).
            // Every other slot either has a single indexed candidate or no opaque one, and keeps
            // the gendered-first order it had.
            if names.len() > 1 {
                let opaque = names.iter().position(|n| {
                    indexed_texture(self.mskins.as_ref(), n)
                        .or_else(|| avatar_texture(&mut self.mskin_index, n))
                        .is_some_and(|t| is_fully_opaque(&t))
                });
                if let Some(i) = opaque {
                    names.swap(0, i);
                }
            }
            // Every candidate this slot could have taken, not just the one it did. Which sheet the
            // client dresses a race in is the open question, and it is answerable only by looking
            // at all of them beside the reference capture.
            if let Some(dir) = std::env::var_os("CAER_DUMP_SKIN") {
                let dir = std::path::PathBuf::from(dir).join("candidates");
                for cand in &names {
                    if let Some(t) = indexed_texture(self.mskins.as_ref(), cand)
                        .or_else(|| avatar_texture(&mut self.mskin_index, cand))
                    {
                        let stem = cand.trim_end_matches(".dds").to_ascii_lowercase();
                        let tag = self.mskins.as_ref().map_or("?", |m| {
                            if m.indexed(cand) {
                                "indexed"
                            } else {
                                "leftover"
                            }
                        });
                        dump_sheet(&dir, &format!("{stem}__{tag}"), &t);
                    }
                }
            }
            for name in names {
                // The table's archive first, the sweep only for a name it does not carry.
                if let Some(t) = indexed_texture(self.mskins.as_ref(), &name)
                    .or_else(|| avatar_texture(&mut self.mskin_index, &name))
                {
                    // The skinned shader alpha-cuts every texel under 0.4 (`skinned.wgsl`), which
                    // hair genuinely needs. Clothing art carries alpha in the regions where BARE
                    // SKIN is meant to show — a short sleeve leaves the forearm transparent — so
                    // the same cutout punches holes straight through the body. That is the
                    // "Briton has no arms" report: the mesh is present and correctly posed, the
                    // texture is bound, and every texel of the forearm is being discarded.
                    //
                    // These slots are never see-through on a real character, so force their alpha
                    // opaque and cache under a distinct key, leaving the original available to
                    // anything that does want the cutout.
                    // **The alpha on these sheets is not transparency, and nothing goes under
                    // them.** Force it opaque and draw the art as authored.
                    //
                    // Two layers were tried underneath and both are wrong, each measured against
                    // the reference bank across 21 bodies:
                    //
                    // - The client's tier-a cloth set (`a_cth1_*`) is GREEN. Compositing over it
                    //   put green arms on Half Ogre and Troll, green legs on Briton, teal patches
                    //   on Inconnu. Eden shows none of them.
                    // - The character's skin tone, from `fig3skincolormap` -> `fig3tints`, is
                    //   **`255,255,255` at tone 1 for every race**. Those tints are MULTIPLIERS
                    //   with white as their identity, not a colour to blend against; using one as
                    //   a backing layer washes every limb toward white, which is what it did.
                    //
                    // `cthArms01_SS_Bri` is a fully painted forearm and `cthLegs01_SS_Bri` fully
                    // painted leggings. The art is complete; the alpha channel is carrying
                    // something this draw does not consume.
                    if let Some(dir) = std::env::var_os("CAER_DUMP_SKIN") {
                        let dir = std::path::PathBuf::from(dir);
                        let stem = name.trim_end_matches(".dds").to_ascii_lowercase();
                        dump_sheet(&dir, &format!("{stem}__overlay"), &t);
                    }
                    let (key, t) = (format!("{name}#opaque"), force_opaque(&t).unwrap_or(t));
                    if std::env::var_os("CAER_SKIN_REPORT").is_some() {
                        println!(
                            "caer-render: skin slot {slot:?} part {} <- {name} => {key} ({}x{}, {:?})",
                            part.name, t.width, t.height, t.format
                        );
                    }
                    part.texture = Some(key.clone());
                    part_textures.insert(key, t);
                    bound += 1;
                    break;
                }
            }
        }
        bound
    }

    /// Overlay equipment-driven player armour textures (MS-02b).
    ///
    /// Each `EquipmentUpdate` item `model` is an `objects.csv` id. That row's Body/Arms/… columns
    /// are `pskins.csv` ids naming DDS members under `items/pskins/`. Those textures replace the
    /// creature-table (or white) binding on matching mesh parts so studded vs plate is visually
    /// legible — not merely a silhouette change.
    ///
    /// Unresolved lookups leave the prior binding in place and are logged (honest placeholder),
    /// never inventing a substitute texture.
    fn assign_equipment_skins(
        &self,
        equip: &caer_protocol::equipment::EquipmentUpdate,
        batch: &mut terrain::SkinnedBatch,
        part_textures: &mut HashMap<String, caer_assets::dds::DdsTexture>,
        female: bool,
    ) -> usize {
        let Some(table) = self.object_skins.as_ref() else {
            return 0;
        };
        let mut bound = 0usize;
        let mut missing: std::collections::HashSet<String> = std::collections::HashSet::new();
        // Slot → texture key for everything this equipment set contributes.
        let mut slot_key: HashMap<SkinSlot, String> = HashMap::new();
        for item in &equip.items {
            for (slot, skin) in table.skins_for_object(item.model, female) {
                let key = format!("pskin:{}#{}", skin.archive, skin.dds);
                if missing.contains(&key) {
                    continue;
                }
                if !part_textures.contains_key(&key) {
                    match load_pskin(skin) {
                        Some(t) => {
                            part_textures.insert(key.clone(), t);
                        }
                        None => {
                            log::debug!(
                                "caer-render: MS-02b — unresolved pskin {} (object {} slot {:?})",
                                skin.dds,
                                item.model,
                                slot
                            );
                            missing.insert(key);
                            continue;
                        }
                    }
                }
                slot_key.insert(slot, key);
            }
        }
        for part in &mut batch.parts {
            let Some(slot) = SkinSlot::from_part_name(&part.name) else {
                continue;
            };
            let Some(key) = slot_key.get(&slot) else {
                continue;
            };
            part.texture = Some(key.clone());
            bound += 1;
        }
        bound
    }

    #[allow(dead_code)]
    #[must_use]
    pub fn starter_armor_model(&self) -> Option<u16> {
        self.object_skins
            .as_ref()
            .and_then(|t| t.starter_armor_object())
    }

    /// The creature tables this resolver was built from.
    ///
    /// Exposed so a harness can sweep every `monsters.csv` model instead of the handful the
    /// product path happens to spawn. See `tests/npc_contact_sheet.rs`.
    #[must_use]
    pub fn creatures(&self) -> &MonsterModels {
        &self.resolver
    }

    /// The model's authored-size correction from `monsters.csv` (`Scale` / 100); 1.0 when unknown.
    #[must_use]
    pub fn model_scale(&self, model_id: u16) -> f32 {
        self.resolver.model_scale(model_id)
    }

    /// Height of an assembled mesh in model units, foot to crown, if it has been uploaded.
    #[must_use]
    pub fn model_height(&self, model_id: u16) -> Option<f32> {
        self.heights.get(&model_id).copied()
    }

    fn remember_resident_mesh(
        &mut self,
        gpu_id: u16,
        model_id: u16,
        tiers: ArmourTiers,
        skin: EquipSkinKey,
        appearance: AvatarAppearance,
    ) {
        self.resident_meshes
            .insert(gpu_id, (model_id, tiers, skin, appearance));
    }

    fn evict_resident_mesh(&mut self, gpu: &mut Gpu, gpu_id: u16) {
        gpu.evict_entity_mesh(gpu_id);
        self.rigs.remove(&gpu_id);
        self.heights.remove(&gpu_id);
        self.last_stands.remove(&gpu_id);
        if let Some(key) = self.resident_meshes.remove(&gpu_id) {
            self.tried.remove(&key);
        }
    }

    /// Release GPU meshes that are not used by the current world frame.
    ///
    /// The resolver and asset indexes stay resident, but mesh buffers, palettes, and their owned
    /// texture slots do not grow with every model a character has ever encountered. `live` comes
    /// from the actual instance uploads immediately before the renderer draws them.
    pub fn evict_unseen_meshes(&mut self, gpu: &mut Gpu, live: &HashSet<u16>) {
        let stale: Vec<u16> = self
            .resident_meshes
            .keys()
            .filter(|id| !live.contains(id))
            .copied()
            .collect();
        for gpu_id in stale {
            self.evict_resident_mesh(gpu, gpu_id);
        }
    }

    /// Ensure `model_id`'s mesh is uploaded to `gpu` (once). Returns whether a mesh is available
    /// for it afterwards — `false` means the caller should fall back to a box.
    pub fn ensure_mesh(&mut self, gpu: &mut Gpu, model_id: u16) -> bool {
        self.ensure_mesh_tiers(
            gpu,
            model_id,
            ArmourTiers::default(),
            EquipSkinKey::default(),
            None,
        )
        .is_some()
    }

    /// Like [`Self::ensure_mesh`], but selects armour-tier parts from a decoded `EquipmentUpdate`
    /// and binds equipment-driven pskin textures (MS-02b).
    /// Returns the GPU mesh id to draw (equal to `model_id` for default tiers + no equip skins,
    /// else a synthetic id).
    pub fn ensure_mesh_for_equipment(
        &mut self,
        gpu: &mut Gpu,
        model_id: u16,
        equip: Option<&caer_protocol::equipment::EquipmentUpdate>,
    ) -> Option<u16> {
        let tiers = equip.map(ArmourTiers::from_equipment).unwrap_or_default();
        let skin_key = equip.map(EquipSkinKey::from_equipment).unwrap_or_default();
        self.ensure_mesh_tiers(gpu, model_id, tiers, skin_key, equip)
    }

    fn gpu_id_for(
        &mut self,
        gpu: &mut Gpu,
        model_id: u16,
        tiers: ArmourTiers,
        skin: EquipSkinKey,
        appearance: AvatarAppearance,
    ) -> u16 {
        // An uncustomised character IS the race's base mesh — that is what DOL stores when nobody
        // touched the create form — so it keeps the plain model id and costs no extra upload.
        if tiers == ArmourTiers::default()
            && skin == EquipSkinKey::default()
            && appearance.is_default()
        {
            return model_id;
        }
        let key = (model_id, tiers, skin, appearance);
        if let Some(&id) = self.equipped_ids.get(&key) {
            return id;
        }
        const CAP: usize = 512;
        const BASE: u16 = 0xE000;
        while self.equipped_ids.len() >= CAP {
            if let Some(old) = self.equipped_order.pop_front() {
                if let Some(id) = self.equipped_ids.remove(&old) {
                    self.evict_resident_mesh(gpu, id);
                    self.tried.remove(&old);
                }
            } else {
                break;
            }
        }
        let mut id = self.next_equipped_id.max(BASE);
        let start = id;
        loop {
            if !self.equipped_ids.values().any(|&v| v == id) {
                break;
            }
            id = if id == u16::MAX {
                BASE
            } else {
                id.saturating_add(1).max(BASE)
            };
            if id == start {
                if let Some(old) = self.equipped_order.pop_front() {
                    if let Some(evict) = self.equipped_ids.remove(&old) {
                        self.evict_resident_mesh(gpu, evict);
                        self.tried.remove(&old);
                        id = evict;
                        break;
                    }
                }
                break;
            }
        }
        self.next_equipped_id = if id == u16::MAX {
            BASE
        } else {
            id.saturating_add(1).max(BASE)
        };
        self.equipped_ids.insert(key, id);
        self.equipped_order.push_back(key);
        id
    }

    fn ensure_mesh_tiers(
        &mut self,
        gpu: &mut Gpu,
        model_id: u16,
        tiers: ArmourTiers,
        skin_key: EquipSkinKey,
        equip: Option<&caer_protocol::equipment::EquipmentUpdate>,
    ) -> Option<u16> {
        // Creatures and props have no fig3 look to carry; only assembled player avatars do.
        let appearance = AvatarAppearance::default();
        let gpu_id = self.gpu_id_for(gpu, model_id, tiers, skin_key, appearance);
        if gpu.has_entity_mesh(gpu_id) || gpu.has_skinned_mesh(gpu_id) {
            return Some(gpu_id);
        }
        // First attempt only; a prior failure stays failed (don't reparse every frame).
        if !self.tried.insert((model_id, tiers, skin_key, appearance)) {
            return None;
        }
        let name = self.resolver.nif_name(model_id)?;
        let name = name.to_string(); // own it: `self` is borrowed mutably below
        let path = self.figures.get(&name)?;
        let Ok(bytes) = std::fs::read(path) else {
            return None;
        };
        // Prefer the rigged form posed at an idle frame (A.3.4 — kills the T-pose). Fall back to the
        // static mesh if the NIF carries no skin or no idle clip resolves, so nothing regresses.
        // `CAER_NO_POSE=1` forces the static bind (T-pose) path — the A/B control for the posing work.
        let no_pose = std::env::var_os("CAER_NO_POSE").is_some();
        // GPU-skinned path: upload skin-space geometry once and animate per instance. Falls back
        // to the CPU-baked pose, then to the static mesh, so nothing regresses if either is absent.
        if !no_pose {
            if let Ok(Some(mut rig)) = caer_assets::nif::read_rigged(&bytes) {
                keep_one_head_variant(&mut rig);
                keep_armour_tiers(&mut rig, tiers);
                if let Some(clip) = self.resolve_pose_clip(model_id, &name) {
                    let mut batch = terrain::skinned_batch(&rig);
                    if !batch.indices.is_empty() {
                        let skin = self.resolver.body_skin(model_id).and_then(load_skin);
                        // Give each part its OWN slot skin; the body skin stays the fallback.
                        let mut part_textures = self.assign_part_skins(model_id, &mut batch);
                        // MS-02b: equipment object models override with authored pskins.
                        let eq_bound = if let Some(eq) = equip {
                            self.assign_equipment_skins(eq, &mut batch, &mut part_textures, false)
                        } else {
                            0
                        };
                        log::debug!(
                            "caer-render: model {model_id} ({name}) GPU-skinned tiers={tiers:?} skin={skin_key:?} ({} verts, {} bones/part, clip {:.2}s, {} part skin(s), {eq_bound} equip-bound part(s))",
                            batch.vertices.len(), batch.bone_stride, clip.duration, part_textures.len(),
                        );
                        if gpu.upload_skinned_mesh(
                            gpu_id,
                            &batch,
                            skin.as_ref(),
                            &part_textures,
                            0.0,
                        ) {
                            self.remember_resident_mesh(
                                gpu_id, model_id, tiers, skin_key, appearance,
                            );
                            // Record the creature's height like the avatar path does. Without it
                            // `model_height` is None for every creature, and anything framing a camera
                            // from it silently falls back to a player-sized default — which frames a
                            // rat and a dragon identically and renders most of the bank off-screen.
                            self.heights
                                .insert(gpu_id, batch.bound_max[2] - batch.bound_min[2]);
                            let loco = self.locomotion_clips(model_id);
                            let strides = self.resolver.strides(model_id).unwrap_or_default();
                            self.rigs.insert(
                                gpu_id,
                                SkinnedRig {
                                    rigs: vec![rig],
                                    clip,
                                    walk: loco.walk,
                                    run: loco.run,
                                    back: loco.back,
                                    slide_left: loco.slide_left,
                                    slide_right: loco.slide_right,
                                    stride_walk: strides.walk,
                                    stride_run: strides.run,
                                    stride_back: strides.back,
                                    stride_strafe: strides.strafe,
                                    z_offset: 0.0,
                                },
                            );
                            return Some(gpu_id);
                        }
                    }
                }
            }
        }
        let batch = match caer_assets::nif::read_rigged(&bytes) {
            Ok(Some(mut rig)) if !no_pose => match self.resolve_pose_clip(model_id, &name) {
                Some(clip) => {
                    keep_one_head_variant(&mut rig);
                    keep_armour_tiers(&mut rig, tiers);
                    let t = POSE_FRAME_T.min(clip.duration);
                    log::debug!("caer-render: model {model_id} ({name}) posed from idle clip ({:.2}s @ t={t:.2})", clip.duration);
                    terrain::posed_batch(&rig, &clip, t)
                }
                None => caer_assets::nif::read_model(&bytes)
                    .map(|m| terrain::untextured_batch(&m))
                    .unwrap_or_else(|_| terrain::untextured_batch_multi(&[])),
            },
            _ => match caer_assets::nif::read_model(&bytes) {
                Ok(model) => terrain::untextured_batch(&model),
                Err(_) => return None,
            },
        };
        if batch.indices.is_empty() {
            return None;
        }
        // Resolve the creature's body skin (monsters.csv → skins.csv → figures/skins/skinNNN.mpk).
        // A miss just leaves the mesh on the white fallback — still an upgrade over the box.
        let skin = self.resolver.body_skin(model_id).and_then(load_skin);
        gpu.upload_entity_mesh(gpu_id, &batch, skin.as_ref());
        self.remember_resident_mesh(gpu_id, model_id, tiers, skin_key, appearance);
        Some(gpu_id)
    }
}

/// Drop all but the first head/hair variant from a creature mesh.
///
/// Humanoid meshes ship FIVE alternative heads and five hairstyles (`HeadA1..5`, `HeadB1..5`) and
/// the client shows one of each, chosen from the NPC's appearance. We have no appearance data yet,
/// so we were drawing all ten interpenetrating — which is why every guard's face read as a mottled
/// patch of cloth rather than a face.
///
/// Scoped to HEADS on purpose. Body/arm/leg/boot tiers are handled by [`keep_armour_tiers`].
fn keep_one_head_variant(rig: &mut caer_assets::nif::RiggedModel) {
    rig.parts.retain(|p| {
        let n = p.name.to_ascii_lowercase();
        let Some(rest) = n.strip_prefix("heada").or_else(|| n.strip_prefix("headb")) else {
            return true; // not a head part
        };
        // Keep variant 1 (and any of its `b` sub-parts); drop 2..n.
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.is_empty() || digits == "1"
    });
}

/// Fingerprint of equipped object models that drive pskin textures (MS-02b cache key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EquipSkinKey {
    pub torso: u16,
    pub arms: u16,
    pub legs: u16,
    pub feet: u16,
    pub hands: u16,
    pub cloak: u16,
    pub helm: u16,
    pub righthand: u16,
    pub lefthand: u16,
    pub twohand: u16,
    pub ranged: u16,
}

impl EquipSkinKey {
    #[must_use]
    pub fn from_equipment(e: &caer_protocol::equipment::EquipmentUpdate) -> Self {
        use caer_protocol::equipment::slot;
        let m = |s: u8| e.item(s).map(|i| i.model).unwrap_or(0);
        Self {
            torso: m(slot::TORSO),
            arms: m(slot::ARMS),
            legs: m(slot::LEGS),
            feet: m(slot::FEET),
            hands: m(slot::HANDS),
            cloak: m(slot::CLOAK),
            helm: m(slot::HELM),
            righthand: m(slot::RIGHTHAND),
            lefthand: m(slot::LEFTHAND),
            twohand: m(slot::TWOHAND),
            ranged: m(slot::RANGED),
        }
    }
}

/// Per-slot armour tier selected by `EquipmentUpdate` extensions (0x15).
///
/// Mythic NIFs ship several numbered `BodyN` / `ArmsN` / `LegsN` / `BootsN` parts; the extension
/// byte on the matching equipment slot picks which one is visible. Tier `0` is treated as `1`
/// (the naked/base layer) so a missing update does not draw every layer at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArmourTiers {
    pub body: u8,
    pub arms: u8,
    pub legs: u8,
    pub boots: u8,
}

impl Default for ArmourTiers {
    fn default() -> Self {
        Self {
            body: 1,
            arms: 1,
            legs: 1,
            boots: 1,
        }
    }
}

impl ArmourTiers {
    /// Build tiers from a decoded 0x15. Absent slots fall back to 1.
    #[must_use]
    pub fn from_equipment(e: &caer_protocol::equipment::EquipmentUpdate) -> Self {
        use caer_protocol::equipment::slot;
        let tier = |s: u8| {
            e.item(s)
                .and_then(|i| i.extension)
                .map(|x| if x == 0 { 1 } else { x })
                .unwrap_or(1)
        };
        Self {
            body: tier(slot::TORSO),
            arms: tier(slot::ARMS),
            legs: tier(slot::LEGS),
            boots: tier(slot::FEET),
        }
    }
}

/// Keep only the armour-tier parts matching `tiers`. Non-armour parts are untouched.
pub fn keep_armour_tiers(rig: &mut caer_assets::nif::RiggedModel, tiers: ArmourTiers) {
    rig.parts.retain(|p| armour_part_kept(&p.name, tiers));
}

fn armour_part_kept(name: &str, tiers: ArmourTiers) -> bool {
    let n = name.to_ascii_lowercase();
    let (prefix, want) = if let Some(rest) = n.strip_prefix("body") {
        (rest, tiers.body)
    } else if let Some(rest) = n.strip_prefix("arms") {
        (rest, tiers.arms)
    } else if let Some(rest) = n.strip_prefix("legs") {
        (rest, tiers.legs)
    } else if let Some(rest) = n.strip_prefix("boots") {
        (rest, tiers.boots)
    } else {
        return true;
    };
    let digits: String = prefix.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return true;
    }
    digits.parse::<u8>().ok() == Some(want)
}

/// Largest per-vertex distance between a rigged part's BIND-pose reconstruction and the same part's
/// static mesh. Zero (up to float noise) when the rig's skeleton is the one the skin was authored
/// against; large when it isn't. This is the avatar's correctness gate — see
/// [`MAX_AVATAR_BIND_ERROR`].
/// A copy of `tex` with every texel fully opaque, as tightly-packed RGBA8.
///
/// Body and limb slots are solid on a real character; only hair and equipment cut out. Returning
/// `None` leaves the caller with the original rather than a silently wrong texture.
fn force_opaque(tex: &caer_assets::dds::DdsTexture) -> Option<caer_assets::dds::DdsTexture> {
    let (w, h, mut rgba) = tex.rgba8_mip0()?;
    for px in rgba.chunks_exact_mut(4) {
        px[3] = 255;
    }
    Some(caer_assets::dds::DdsTexture {
        width: w,
        height: h,
        format: caer_assets::dds::DdsFormat::Rgba8,
        mips: vec![rgba],
    })
}

/// A copy of `tex` flooded with one colour, alpha untouched so cutouts still cut.
fn flood_colour(
    tex: &caer_assets::dds::DdsTexture,
    rgb: [u8; 3],
) -> Option<caer_assets::dds::DdsTexture> {
    let (w, h, mut rgba) = tex.rgba8_mip0()?;
    for px in rgba.chunks_exact_mut(4) {
        px[..3].copy_from_slice(&rgb);
    }
    Some(caer_assets::dds::DdsTexture {
        width: w,
        height: h,
        format: caer_assets::dds::DdsFormat::Rgba8,
        mips: vec![rgba],
    })
}

/// Magenta. Nothing in DAoC's art is this colour, which is the entire point.
const FILLER_PROBE_RGB: [u8; 3] = [255, 0, 255];

/// A copy of `tex` with every atlas-filler texel painted [`FILLER_PROBE_RGB`].
///
/// Renders the question "do this part's triangles sample the space between the islands?" as an
/// answer a screenshot can give. Measuring it in UV space instead was a dead end: on a sheet like
/// `CthGloves01_SS_Dwa.dds`, the space between islands is packed with the glove's own leather, so
/// two thirds of the part's UV area lands on "filler" and the glove renders perfectly. Filler that
/// matches the material is invisible; filler that does not is a grey wedge on a boot. Only the
/// render can tell those apart, so let it.
fn paint_filler(tex: &caer_assets::dds::DdsTexture) -> Option<caer_assets::dds::DdsTexture> {
    let (w, h, mut rgba) = tex.rgba8_mip0()?;
    let mask = crate::uv_coverage::atlas_mask(w, h, &rgba);
    mask.background?;
    for (i, px) in rgba.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i as u32 % w, i as u32 / w);
        if !mask.art_at(x, y) {
            px[..3].copy_from_slice(&FILLER_PROBE_RGB);
        }
    }
    Some(caer_assets::dds::DdsTexture {
        width: w,
        height: h,
        format: caer_assets::dds::DdsFormat::Rgba8,
        mips: vec![rgba],
    })
}

fn bind_error(rig: &caer_assets::nif::RiggedModel, stat: &caer_assets::nif::Model) -> f32 {
    let mut worst = 0.0f32;
    for (pi, part) in rig.parts.iter().enumerate() {
        // Pair by identity, not by index. The two readers do not agree on a NIF's part list:
        // `sar_f_head01` has 11 static shapes (ten 24-vertex `Box` helpers plus the 541-vertex
        // head) and exactly one rigged shape. Indexing pitted the head's skinned vertices against
        // a Box and reported 14.58 for a head that binds at 0.00 — a defect in this measurement,
        // not in the asset. Fall back to the index only when nothing matches, so single-shape
        // meshes behave exactly as before.
        // Match on vertex count, not name: `stamp_equip_names` has already rewritten the rigged
        // part's name to a SkinSlot label ("HeadA"), so it no longer matches the static shape it
        // came from. Only an unambiguous count is trusted.
        let mut same: Vec<&caer_assets::nif::MeshPart> = stat
            .parts
            .iter()
            .filter(|s| s.positions.len() == part.positions.len())
            .collect();
        let sp = if same.len() == 1 {
            Some(same.remove(0))
        } else {
            stat.parts.get(pi)
        };
        let Some(sp) = sp else {
            continue;
        };
        for vi in 0..part.positions.len() {
            let Some(s) = sp.positions.get(vi) else {
                continue;
            };
            let b = rig.bind_position(pi, vi);
            let d = ((b[0] - s[0]).powi(2) + (b[1] - s[1]).powi(2) + (b[2] - s[2]).powi(2)).sqrt();
            worst = worst.max(d);
        }
    }
    worst
}

fn tint_untextured(
    batch: &mut terrain::SkinnedBatch,
    textures: &HashMap<String, caer_assets::dds::DdsTexture>,
    rgb: [u8; 3],
) {
    let c = [
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
    ];
    let ranges: Vec<(usize, usize)> = batch
        .parts
        .iter()
        .filter(|part| {
            !part
                .texture
                .as_deref()
                .is_some_and(|n| textures.contains_key(n))
        })
        .map(|part| {
            (
                part.start as usize,
                (part.end as usize).min(batch.indices.len()),
            )
        })
        .collect();
    for (start, end) in ranges {
        for i in start..end {
            let idx = batch.indices[i] as usize;
            if let Some(v) = batch.vertices.get_mut(idx) {
                v.color = c;
            }
        }
    }
}

/// Decode a resolved monster skin: open `figures/skins/skin{archive:03}.mpk`, find the DDS member
/// (case-insensitive, the `.tga`/`.bmp`→`.dds` swap already applied by the resolver), and decode it.
/// Returns `None` on any miss so the caller falls back to the white bind.
/// Find a player-avatar texture by DDS file name, across `figures/Mskins/*.mpk`.
///
/// Avatar textures do NOT live with the creature skins (`figures/skins/`, which is keyed by
/// `skins.csv`) and are not all named inside their NIFs: the head's texture is `<part>.dds`
/// matching its mesh name (`hig_f_head01.nif` → `hig_f_head01.dds` in `mskin004.mpk`), while the
/// hair's IS named in the NIF (`nor_f_hair01_blonde.dds` — a Norse texture reused across races).
/// So both cases resolve by NAME here, over the whole Mskins set.
///
/// The archive→member index is built once and cached: there are ~69 archives, and rescanning them
/// per part per avatar would dominate load time.
/// Decode a sheet from the archive `mskins.csv` names for it, if the table has a row.
///
/// **The table names the archive outright, and the sweep below does not read it.** Several sheets
/// ship more than once under one name with different art: `cthLegs01_ss_Hig_M.dds` is in both
/// `mskin014` (DXT1) and `mskin038` (DXT5), and `cthArms01_SS_Bri_M.dds` in `mskin011` and
/// `mskin037` at max alpha 210 and 102. The swept index resolves those by sorted archive order,
/// which is not a rule the client has — it is whichever file happens to sort first. Highlander
/// male's legs and his arms came out of different copies and read as two different skin tones,
/// which is exactly what that looks like on screen.
fn indexed_texture(
    mskins: Option<&caer_assets::mskins::Mskins>,
    name: &str,
) -> Option<caer_assets::dds::DdsTexture> {
    let row = mskins?.get(name)?;
    let arc = terrain::client_root().join(row.archive_rel());
    let dds = caer_assets::open_member(&arc, name).ok()??;
    caer_assets::dds::read_model_dds(&dds).ok()
}

fn avatar_texture(
    index: &mut Option<HashMap<String, std::path::PathBuf>>,
    name: &str,
) -> Option<caer_assets::dds::DdsTexture> {
    let idx = index.get_or_insert_with(|| {
        let mut map = HashMap::new();
        // Mskins first, then skins: the race OVERLAYS live in Mskins and the client's BASE cloth
        // set (`a_cth1_*`, `b_cth1_*`) lives in `figures/skins`. Both are needed, and Mskins wins
        // a name collision because that is the client's own search order.
        let dirs = [
            terrain::client_root().join("figures/Mskins"),
            terrain::client_root().join("figures/skins"),
        ];
        for dir in dirs {
            if let Ok(rd) = std::fs::read_dir(&dir) {
                let mut archives: Vec<_> = rd.flatten().map(|e| e.path()).collect();
                archives.sort();
                for arc in archives {
                    // Directory only — the index needs names, not pixels, and inflating every member
                    // of every Mskins archive to build it cost minutes of startup for nothing.
                    let Ok(names) = caer_assets::list_names(&arc) else {
                        continue;
                    };
                    for name in names {
                        // First archive wins, matching the client's search order closely enough.
                        map.entry(name.to_ascii_lowercase())
                            .or_insert_with(|| arc.clone());
                    }
                }
            }
        }
        log::debug!(
            "caer-render: avatar textures — indexed {} Mskins/skins entries",
            map.len()
        );
        map
    });
    let key = name.to_ascii_lowercase();
    let arc = idx.get(&key)?;
    let dds = caer_assets::open_member(arc, name).ok()??;
    caer_assets::dds::read_model_dds(&dds).ok()
}

/// Is every texel of this sheet fully opaque?
///
/// The discriminator between a garment and bare-limb art: the starting-set clothing sheets are
/// solid, and the sheets that paint an arm or a leg carry alpha where the skin below shows.
fn is_fully_opaque(tex: &caer_assets::dds::DdsTexture) -> bool {
    tex.rgba8_mip0()
        .is_some_and(|(_, _, px)| px.chunks_exact(4).all(|p| p[3] == 255))
}

/// Write one decoded sheet to `<dir>/<stem>.png`, for looking at what actually got bound.
///
/// A material defect is an image question: a sheet that binds, decodes and covers the mesh can
/// still be somebody else's clothing, and every count in this file scores that identically to the
/// right one. Driven by `CAER_DUMP_SKIN=<dir>`; writes nothing unless it is set.
fn dump_sheet(dir: &std::path::Path, stem: &str, tex: &caer_assets::dds::DdsTexture) {
    let Some((w, h, px)) = tex.rgba8_mip0() else {
        return;
    };
    let _ = std::fs::create_dir_all(dir);
    let Ok(file) = std::fs::File::create(dir.join(format!("{stem}.png"))) else {
        return;
    };
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    if let Ok(mut w) = enc.write_header() {
        let _ = w.write_image_data(&px);
    }
}

fn load_skin(skin: &caer_assets::monsters::SkinRef) -> Option<caer_assets::dds::DdsTexture> {
    let archive = terrain::client_root().join(format!("figures/skins/skin{:03}.mpk", skin.archive));
    let dds = caer_assets::open_member(&archive, &skin.dds).ok()??;
    caer_assets::dds::read_model_dds(&dds).ok()
}

/// Decode a player-armour pskin from `items/pskins/pskinNNN.mpk` (MS-02b).
fn load_pskin(skin: &caer_assets::pskins::PskinRef) -> Option<caer_assets::dds::DdsTexture> {
    let archive = terrain::client_root().join(skin.archive_path());
    let dds = caer_assets::open_member(&archive, &skin.dds).ok()??;
    caer_assets::dds::read_model_dds(&dds).ok()
}

#[cfg(test)]
mod starting_set_tests {
    use super::*;
    use caer_assets::dds::{DdsFormat, DdsTexture};

    fn sheet(w: u32, h: u32, px: [u8; 4]) -> DdsTexture {
        DdsTexture {
            width: w,
            height: h,
            format: DdsFormat::Rgba8,
            mips: vec![px.repeat((w * h) as usize)],
        }
    }

    /// Opacity is the discriminator between a GARMENT and bare-limb art, so it has to be exact.
    /// The client ships both spellings for several race+slot pairs — `cthLegs01_SS_Bri` is solid
    /// leggings and `cthLegs01_SS_bri_F` is a painted bare leg at alpha 62 — and taking the
    /// gendered one first is what left Briton female with no pants.
    #[test]
    fn a_garment_is_opaque_and_bare_limb_art_is_not() {
        assert!(is_fully_opaque(&sheet(4, 4, [10, 20, 30, 255])));
        assert!(!is_fully_opaque(&sheet(4, 4, [10, 20, 30, 62])));

        // One translucent texel is enough: a sheet that is solid except where the forearm shows
        // is exactly the art this has to reject.
        let mut mixed = sheet(2, 2, [10, 20, 30, 255]);
        mixed.mips[0][7] = 200;
        assert!(
            !is_fully_opaque(&mixed),
            "a single translucent texel makes it bare-limb art"
        );
    }

    /// `force_opaque` is what the starting-set sheets are drawn through, so it must change alpha
    /// and nothing else. Two layers were tried underneath these sheets and both were wrong; the
    /// art is complete on its own, and anything that alters its colour here is a third mistake.
    #[test]
    fn forcing_opaque_keeps_the_art_and_only_lifts_alpha() {
        let src = sheet(2, 2, [90, 70, 54, 62]);
        let out = force_opaque(&src).expect("forces");
        let (_, _, px) = out.rgba8_mip0().expect("decodes");
        assert_eq!(&px[0..3], &[90, 70, 54], "the art must survive untouched");
        assert_eq!(px[3], 255, "alpha is lifted");
    }
}

#[cfg(test)]
mod equipment_tier_tests {
    use super::*;

    #[test]
    fn default_tiers_are_one() {
        assert_eq!(
            ArmourTiers::default(),
            ArmourTiers {
                body: 1,
                arms: 1,
                legs: 1,
                boots: 1
            }
        );
    }

    #[test]
    fn armour_part_filter_keeps_matching_tier() {
        let t = ArmourTiers {
            body: 3,
            arms: 1,
            legs: 2,
            boots: 1,
        };
        assert!(armour_part_kept("Body3", t));
        assert!(!armour_part_kept("Body1", t));
        assert!(armour_part_kept("Arms1", t));
        assert!(!armour_part_kept("Arms2", t));
        assert!(armour_part_kept("Legs2", t));
        assert!(armour_part_kept("HeadA1", t), "non-armour parts stay");
    }

    /// REQ-020 precursor: two equipment payloads select different visible part sets.
    #[test]
    fn different_equipment_selects_different_parts() {
        use caer_protocol::codec::PacketWriter;
        use caer_protocol::equipment::{decode, slot};
        fn equip(torso_ext: u8) -> caer_protocol::equipment::EquipmentUpdate {
            let mut w = PacketWriter::new();
            w.u16(42).u8(0).u8(0).u8(0).u8(0).u8(1);
            // torso with extension, no texture/effect
            w.u8(slot::TORSO).u16(0x0100).u8(torso_ext);
            decode(w.as_slice()).unwrap()
        }
        let a = ArmourTiers::from_equipment(&equip(1));
        let b = ArmourTiers::from_equipment(&equip(4));
        assert_ne!(a.body, b.body);
        assert!(armour_part_kept("Body1", a));
        assert!(!armour_part_kept("Body1", b));
        assert!(armour_part_kept("Body4", b));
    }
}

#[cfg(test)]
mod facial_morph_tests {
    use super::*;
    use caer_assets::nif::{MorphAnim, MorphTarget};

    fn head_morph() -> MorphAnim {
        // Target zero is the rest shape.  The retail head's static targets are unkeyed, which is
        // deliberately distinct from the time-driven scenery morphs the renderer also supports.
        MorphAnim {
            relative: true,
            targets: vec![
                MorphTarget::default(),
                MorphTarget {
                    deltas: vec![[1.0, 0.0, 0.0]], // FM1, Nose left
                    ..MorphTarget::default()
                },
                MorphTarget {
                    deltas: vec![[0.0, 2.0, 0.0]], // FM2, Nose right
                    ..MorphTarget::default()
                },
                MorphTarget {
                    deltas: vec![[0.0, 0.0, 50.0]], // FM9 is not a stock UI slider
                    ..MorphTarget::default()
                },
            ],
        }
    }

    fn appearance_with_nose(tick: u8) -> AvatarAppearance {
        AvatarAppearance {
            // The high nibble is the source Nose slider.  The other source sliders remain
            // neutral, just as a retail customizer does before the player moves them.
            eye_size: tick << 4,
            lip_size: 0x44,
            facial_morphs_enabled: true,
            ..AvatarAppearance::default()
        }
    }

    /// The four source sliders choose their own tagged min/max pairs, with no accidental target
    /// order assumption and no invented control for FM9+.  This is a known-bad regression case:
    /// treating target index as a UI slider would move the face 50 units along Z here.
    #[test]
    fn source_tagged_facial_morphs_blend_only_the_selected_pair() {
        let morph = head_morph();
        let ids = [None, Some(1), Some(2), Some(9)];

        let mut historical = vec![[10.0, 20.0, 30.0]];
        apply_facial_morph_targets(
            &mut historical,
            Some(&morph),
            &ids,
            AvatarAppearance::default(),
        );
        assert_eq!(
            historical,
            vec![[10.0, 20.0, 30.0]],
            "a legacy all-zero packet is the neutral, uncustomized head"
        );

        let mut neutral = vec![[10.0, 20.0, 30.0]];
        apply_facial_morph_targets(&mut neutral, Some(&morph), &ids, appearance_with_nose(4));
        assert_eq!(neutral, vec![[10.0, 20.0, 30.0]]);

        let mut left = vec![[10.0, 20.0, 30.0]];
        apply_facial_morph_targets(&mut left, Some(&morph), &ids, appearance_with_nose(0));
        assert_eq!(left, vec![[11.0, 20.0, 30.0]]);

        let mut right = vec![[10.0, 20.0, 30.0]];
        apply_facial_morph_targets(&mut right, Some(&morph), &ids, appearance_with_nose(8));
        assert_eq!(right, vec![[10.0, 22.0, 30.0]]);
    }
}

#[cfg(test)]
mod avatar_hair_source_tests {
    use super::*;

    /// A selected hair-style byte names a `fig3map` PART_HAIR variant, not a mesh inferred from
    /// the colour sheet. Briton male is the useful retail falsifier: style four's table-owned
    /// mesh is `bri_m_hair04`, while its colour sheet is shared with Hair 1. The old heuristic
    /// therefore rendered Hair 1 and made several later styles appear to be missing.
    #[test]
    fn selected_hair_uses_exact_fig3_variant_and_bald_omits_the_base_mesh() {
        let root = caer_assets::client_dep::required_caer_client_root(
            "selected_hair_uses_exact_fig3_variant_and_bald_omits_the_base_mesh",
        );
        let figures = FigureModels::load(root.join("gamedata.mpk"))
            .expect("load retail fig3 map and parts tables");

        assert_eq!(
            source_hair_mesh(&figures, 1, caer_assets::figures::GENDER_MALE, 0,),
            HairMeshSelection::Base,
            "stored zero retains retail's first/base hair part"
        );
        match source_hair_mesh(&figures, 1, caer_assets::figures::GENDER_MALE, 4) {
            HairMeshSelection::Source(part) => {
                assert_eq!(part.filename, "bri_m_hair04");
                assert_eq!(part.archive, 7);
            }
            other => panic!("Briton Hair 4 must select its exact fig3 variant, got {other:?}"),
        }
        match source_hair_mesh(&figures, 1, caer_assets::figures::GENDER_MALE, 8) {
            HairMeshSelection::Source(part) => assert_eq!(part.filename, "bri_m_hair09"),
            other => panic!("Briton Hair 8 must select its exact fig3 variant, got {other:?}"),
        }
        assert_eq!(
            source_hair_mesh(&figures, 1, caer_assets::figures::GENDER_MALE, 7),
            HairMeshSelection::Omit,
            "retail's explicit Bald hole must not fall back to Hair 1"
        );
    }
}
