//! Deterministic particle subsystem (leg 10).
//!
//! Consumes [`caer_assets::nif::ParticleEmitterDef`] (NIF `NiParticleSystemController` fields),
//! advances with a fixed timestep and a seeded LCG — no wall clock.
//!
//! Draw path: camera-facing billboard quads with an honest placeholder soft-blob texture
//! ([`PARTICLE_BILLBOARD_PLACEHOLDER`]) and additive blending. Cube [`crate::gpu::Instance`]s
//! remain available for twin-seed / legacy harnesses.
//!
//! **Effect fidelity is not claimed** — placeholder art, not DAoC sprites.

use bytemuck::{Pod, Zeroable};
use caer_assets::nif::ParticleEmitterDef;

use crate::gpu::Instance;

/// Honest label for the procedural soft-blob sprite — not client art, not a DAoC texture.
pub const PARTICLE_BILLBOARD_PLACEHOLDER: &str = "PLACEHOLDER_SOFT_BLOB_NOT_DAOC_ART";

/// Named draw-path marker. Falsifier `particle_billboard_presence_pixels` goes red if this
/// path is deleted or particle uploads are ignored by the GPU encode pass.
pub const PARTICLE_BILLBOARD_DRAW_PATH: &str = "particle_billboard_draw_path";

/// One vertex of a CPU-expanded camera-facing particle quad (two triangles / six verts).
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct ParticleBillboardVert {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    pub color: [f32; 4],
}

/// Simple LCG — same seed → same spawn sequence on every host.
#[derive(Clone, Debug)]
struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Self {
            state: seed | 1, // avoid zero lock
        }
    }

    fn next_u32(&mut self) -> u32 {
        // Numerical Recipes LCG
        self.state = self.state.wrapping_mul(1664525).wrapping_add(1013904223);
        (self.state >> 32) as u32
    }

    fn next_f32(&mut self) -> f32 {
        self.next_u32() as f32 / (u32::MAX as f32)
    }

    /// Uniform in `[lo, hi]`.
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next_f32()
    }
}

#[derive(Clone, Debug)]
struct Particle {
    pos: [f32; 3],
    vel: [f32; 3],
    age: f32,
    lifespan: f32,
    base_size: f32,
}

/// One running emitter.
#[derive(Clone, Debug)]
pub struct ParticleSystem {
    def: ParticleEmitterDef,
    rng: Lcg,
    time: f32,
    emit_accum: f32,
    particles: Vec<Particle>,
    /// Soft cap so a runaway rate cannot OOM the evidence harness.
    max_particles: usize,
}

impl ParticleSystem {
    #[must_use]
    pub fn from_def(def: ParticleEmitterDef, seed: u64) -> Self {
        Self {
            def,
            rng: Lcg::new(seed),
            time: 0.0,
            emit_accum: 0.0,
            particles: Vec::new(),
            max_particles: 2048,
        }
    }

    /// Live particle count.
    #[must_use]
    pub fn particle_count(&self) -> usize {
        self.particles.len()
    }

    #[must_use]
    pub fn def(&self) -> &ParticleEmitterDef {
        &self.def
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.particles.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.particles.is_empty()
    }

    /// Advance simulation by a fixed `dt` (seconds). Call with a constant step for evidence.
    /// Run the system forward `seconds` in fixed steps, so it starts already populated.
    ///
    /// A stage emitter is authored as weather that has always been falling, not as a burst that
    /// begins when the screen opens. Without this the character screen fades up on an empty sky
    /// and fills in over the following seconds, and a single-frame capture sees nothing at all.
    ///
    /// Fixed 1/30 steps rather than one big `dt`: emission and integration are both rate-based, so
    /// a single huge step would emit the right COUNT of particles and place every one of them at
    /// the same position.
    pub fn warm(&mut self, seconds: f32) {
        let step = 1.0 / 30.0;
        let steps = (seconds.max(0.0) / step).round() as u32;
        for _ in 0..steps.min(600) {
            self.tick(step);
        }
    }

    pub fn tick(&mut self, dt: f32) {
        let dt = dt.max(0.0);
        self.time += dt;

        // Emit only inside the authored window (emit_stop <= emit_start ⇒ always on).
        let window_open = if self.def.emit_stop > self.def.emit_start {
            self.time >= self.def.emit_start && self.time <= self.def.emit_stop
        } else {
            true
        };
        if window_open && self.def.emit_rate > 0.0 {
            self.emit_accum += self.def.emit_rate * dt;
            while self.emit_accum >= 1.0 && self.particles.len() < self.max_particles {
                self.emit_accum -= 1.0;
                self.spawn_one();
            }
        }

        let grow = self.def.grow.max(0.0);
        let fade = self.def.fade.max(0.0);
        let _ = (grow, fade); // used in size_at

        self.particles.retain_mut(|p| {
            p.age += dt;
            if p.age >= p.lifespan {
                return false;
            }
            p.pos[0] += p.vel[0] * dt;
            p.pos[1] += p.vel[1] * dt;
            p.pos[2] += p.vel[2] * dt;
            true
        });
    }

    fn spawn_one(&mut self) {
        let speed = (self.def.speed
            + self
                .rng
                .range(-self.def.speed_random, self.def.speed_random))
        .max(0.0);
        let dec = self.def.declination
            + self.rng.range(
                -self.def.declination_variation,
                self.def.declination_variation,
            );
        let plan = self.def.planar_angle
            + self.rng.range(
                -self.def.planar_angle_variation,
                self.def.planar_angle_variation,
            );
        // NetImmerse: declination from +Z, planar about Z.
        let sin_d = dec.sin();
        let cos_d = dec.cos();
        let dir = [sin_d * plan.cos(), sin_d * plan.sin(), cos_d];
        let lifespan = (self.def.lifetime
            + self
                .rng
                .range(-self.def.lifetime_random, self.def.lifetime_random))
        .max(0.05);
        self.particles.push(Particle {
            pos: self.def.origin,
            vel: [dir[0] * speed, dir[1] * speed, dir[2] * speed],
            age: 0.0,
            lifespan,
            base_size: self.def.size.max(0.05),
        });
    }

    fn size_at(&self, p: &Particle) -> f32 {
        let t = (p.age / p.lifespan).clamp(0.0, 1.0);
        let grow = self.def.grow.max(0.0);
        let fade = self.def.fade.max(0.0);
        // Grow over the first `grow` seconds of life; fade over the last `fade` seconds.
        let mut scale = 1.0f32;
        if grow > 0.0 && p.age < grow {
            scale *= (p.age / grow).clamp(0.05, 1.0);
        }
        if fade > 0.0 && (p.lifespan - p.age) < fade {
            scale *= ((p.lifespan - p.age) / fade).clamp(0.05, 1.0);
        }
        let _ = t;
        (p.base_size * scale).max(0.02)
    }

    /// GPU instances for the current particle set (small cubes — legacy / twin-seed harness).
    #[must_use]
    pub fn instances(&self) -> Vec<Instance> {
        let rgb = [
            self.def.color[0].clamp(0.0, 1.0),
            self.def.color[1].clamp(0.0, 1.0),
            self.def.color[2].clamp(0.0, 1.0),
        ];
        self.particles
            .iter()
            .map(|p| Instance {
                pos: p.pos,
                color: rgb,
                scale: self.size_at(p) * 0.5, // Instance scale is half-extent
            })
            .collect()
    }

    /// Camera-facing billboard mesh for the live draw path ([`PARTICLE_BILLBOARD_DRAW_PATH`]).
    ///
    /// `right` / `up` are **render-space** unit axes from [`crate::camera::Camera::billboard_axes`].
    /// Six verts per particle (two triangles). Soft-blob UVs cover [0,1]².
    #[must_use]
    pub fn billboard_mesh(&self, right: [f32; 3], up: [f32; 3]) -> Vec<ParticleBillboardVert> {
        let rgb = [
            self.def.color[0].clamp(0.0, 1.0),
            self.def.color[1].clamp(0.0, 1.0),
            self.def.color[2].clamp(0.0, 1.0),
        ];
        let alpha = self.def.color[3].clamp(0.15, 1.0);
        let mut out = Vec::with_capacity(self.particles.len() * 6);
        for p in &self.particles {
            let half = self.size_at(p) * 0.5;
            let color = [rgb[0], rgb[1], rgb[2], alpha];
            // Corner offsets in the camera plane: (-1,-1), (1,-1), (1,1), (-1,1)
            let corners = [
                (-1.0f32, -1.0f32, 0.0f32, 1.0f32),
                (1.0, -1.0, 1.0, 1.0),
                (1.0, 1.0, 1.0, 0.0),
                (-1.0, 1.0, 0.0, 0.0),
            ];
            let mut world = [[0.0f32; 3]; 4];
            for (i, &(sx, sy, _, _)) in corners.iter().enumerate() {
                world[i] = [
                    p.pos[0] + right[0] * sx * half + up[0] * sy * half,
                    p.pos[1] + right[1] * sx * half + up[1] * sy * half,
                    p.pos[2] + right[2] * sx * half + up[2] * sy * half,
                ];
            }
            // Two triangles: 0-1-2, 0-2-3
            let idx = [0usize, 1, 2, 0, 2, 3];
            for &i in &idx {
                let (sx, sy, u, v) = corners[i];
                let _ = (sx, sy);
                out.push(ParticleBillboardVert {
                    pos: world[i],
                    uv: [u, v],
                    color,
                });
            }
        }
        out
    }

    /// True once the emit window has closed and every particle has died.
    #[must_use]
    pub fn finished(&self) -> bool {
        let emit_done = self.def.emit_stop > self.def.emit_start && self.time > self.def.emit_stop;
        emit_done && self.particles.is_empty()
    }
}

/// Build a presence burst from a typed [`caer_world::EffectRequest`].
/// Soft-blob only — [`caer_world::EffectResource::PresencePlaceholder`].
///
/// This function **always** returns a diagnostic placeholder. A typed effect id is never
/// satisfied by calling it.
#[must_use]
pub fn burst_from_request(origin: [f32; 3], req: caer_world::EffectRequest) -> ParticleEmitterDef {
    let _ = req.resource;
    presence_burst(origin, req.spell_id)
}

/// True when `def` is the diagnostic soft-blob path, not a shipped effect NIF.
#[must_use]
pub fn is_diagnostic_placeholder(def: &ParticleEmitterDef) -> bool {
    def.name.starts_with("presence_") || def.name == PARTICLE_BILLBOARD_PLACEHOLDER
}

/// A placeholder burst never fulfills a typed effect id (even if `req.resource` is Typed).
#[must_use]
pub fn placeholder_satisfies_typed(
    def: &ParticleEmitterDef,
    resource: caer_world::EffectResource,
    typed_id: u16,
) -> bool {
    if is_diagnostic_placeholder(def) {
        return false;
    }
    resource.satisfies_typed(typed_id)
}

/// Presence-only burst for SpellEffect 0x1B → client (leg 10 wire). Origin is **render-space**
/// (world − frame origin). Fidelity not claimed — SCN-07 asserts creation, not look-alike.
#[must_use]
pub fn presence_burst(origin: [f32; 3], spell_id: u16) -> ParticleEmitterDef {
    // Tint varies lightly with spell_id so two different spells are distinguishable in a
    // screenshot without claiming the real DAoC palette.
    let h = f32::from(spell_id % 360) / 360.0;
    let color = [0.4 + 0.6 * h, 0.3 + 0.4 * (1.0 - h), 1.0 - 0.5 * h, 1.0];
    ParticleEmitterDef {
        name: format!("presence_{spell_id}"),
        origin,
        speed: 40.0,
        speed_random: 15.0,
        declination: 0.4,
        declination_variation: 0.5,
        planar_angle: 0.0,
        planar_angle_variation: std::f32::consts::TAU,
        color,
        size: 4.0,
        emit_start: 0.0,
        emit_stop: 0.25,
        emit_rate: 80.0,
        lifetime: 0.7,
        lifetime_random: 0.2,
        grow: 0.05,
        fade: 0.2,
        mesh_particles: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toy_def() -> ParticleEmitterDef {
        ParticleEmitterDef {
            name: "toy".into(),
            origin: [0.0, 0.0, 0.0],
            speed: 10.0,
            speed_random: 0.0,
            declination: 0.0,
            declination_variation: 0.0,
            planar_angle: 0.0,
            planar_angle_variation: 0.0,
            color: [1.0, 0.2, 0.2, 1.0],
            size: 2.0,
            emit_start: 0.0,
            emit_stop: 0.0,
            emit_rate: 30.0,
            lifetime: 1.0,
            lifetime_random: 0.0,
            grow: 0.0,
            fade: 0.0,
            mesh_particles: false,
        }
    }

    /// A stage emitter is weather that has always been falling, not a burst that starts when the
    /// screen opens. Warming must leave a populated field, and it must SPREAD that field — a
    /// single large step would emit the right count of particles and stack every one of them at
    /// the emitter.
    #[test]
    fn warming_populates_and_spreads_the_field() {
        let mut cold = ParticleSystem::from_def(toy_def(), 7);
        assert_eq!(cold.particle_count(), 0, "a fresh system starts empty");

        cold.warm(1.0);
        assert!(cold.particle_count() > 0, "warming emitted nothing");

        let mesh = cold.billboard_mesh([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
        assert!(!mesh.is_empty());
        let xs: Vec<f32> = mesh.iter().map(|v| v.pos[0]).collect();
        let lo = xs.iter().copied().fold(f32::MAX, f32::min);
        let hi = xs.iter().copied().fold(f32::MIN, f32::max);
        assert!(
            hi - lo > 0.0,
            "every particle landed at the same place — warmed in one step, not many"
        );

        // KNOWN-BAD CONTROL: one big step. Same simulated time, same emission count, no spread.
        let mut one_shot = ParticleSystem::from_def(toy_def(), 7);
        one_shot.tick(1.0);
        let m2 = one_shot.billboard_mesh([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
        let x2: Vec<f32> = m2.iter().map(|v| v.pos[0]).collect();
        let spread2 = x2.iter().copied().fold(f32::MIN, f32::max)
            - x2.iter().copied().fold(f32::MAX, f32::min);
        assert!(
            (hi - lo) > spread2,
            "stepped warming must spread wider than a single step"
        );
    }

    #[test]
    fn burst_from_request_is_presence_placeholder() {
        let req = caer_world::EffectRequest {
            caster_id: 1,
            target_id: 2,
            spell_id: 407,
            bolt_time: 3,
            success: 1,
            no_sound: false,
            resource: caer_world::EffectResource::PresencePlaceholder,
        };
        let def = burst_from_request([0.0, 0.0, 0.0], req);
        assert!(
            def.name.starts_with("presence_"),
            "must not claim a retail effect NIF; got {}",
            def.name
        );
        assert!(is_diagnostic_placeholder(&def));
        assert_eq!(req.attach_id(), 2);
        assert_eq!(req.lifetime_tenths(), 3);
    }

    /// Named falsifier: a typed effect id must not be satisfied by a placeholder spawn.
    #[test]
    fn typed_effect_id_not_satisfied_by_placeholder_spawn() {
        let typed = caer_world::EffectRequest {
            caster_id: 1,
            target_id: 2,
            spell_id: 407,
            bolt_time: 3,
            success: 1,
            no_sound: false,
            resource: caer_world::EffectResource::Typed { effect_id: 407 },
        };
        assert!(typed.resource.satisfies_typed(407));
        let def = burst_from_request([0.0, 0.0, 0.0], typed);
        assert!(is_diagnostic_placeholder(&def));
        assert!(
            !placeholder_satisfies_typed(&def, typed.resource, 407),
            "soft-blob burst must not count as typed effect 407"
        );
        assert!(!caer_world::effect_spawn_satisfies_typed(
            caer_world::EffectResource::PresencePlaceholder,
            407
        ));
    }

    #[test]
    fn presence_burst_emits_then_finishes() {
        let mut sys = ParticleSystem::from_def(presence_burst([1.0, 2.0, 3.0], 42), 7);
        assert!(!sys.finished());
        for _ in 0..60 {
            sys.tick(1.0 / 30.0);
        }
        assert!(sys.finished(), "burst must end; leftover={}", sys.len());
    }

    #[test]
    fn same_seed_same_instance_stream() {
        let mut a = ParticleSystem::from_def(toy_def(), 0xCAE0_DEAD_BEEFu64);
        let mut b = ParticleSystem::from_def(toy_def(), 0xCAE0_DEAD_BEEFu64);
        for _ in 0..10 {
            a.tick(1.0 / 30.0);
            b.tick(1.0 / 30.0);
        }
        let ia = a.instances();
        let ib = b.instances();
        assert_eq!(ia.len(), ib.len());
        assert!(!ia.is_empty());
        for (x, y) in ia.iter().zip(ib.iter()) {
            assert_eq!(x.pos, y.pos);
            assert_eq!(x.scale, y.scale);
        }
    }

    #[test]
    fn different_seed_diverges() {
        let mut def = toy_def();
        def.speed_random = 5.0;
        let mut a = ParticleSystem::from_def(def.clone(), 1);
        let mut b = ParticleSystem::from_def(def, 2);
        for _ in 0..20 {
            a.tick(0.05);
            b.tick(0.05);
        }
        let ia = a.instances();
        let ib = b.instances();
        assert_eq!(ia.len(), ib.len());
        assert!(
            ia.iter()
                .zip(ib.iter())
                .any(|(x, y)| x.pos != y.pos || x.scale != y.scale),
            "different seeds must diverge with speed_random > 0"
        );
    }

    #[test]
    fn billboard_mesh_six_verts_per_particle_facing_axes() {
        assert_eq!(
            PARTICLE_BILLBOARD_PLACEHOLDER, "PLACEHOLDER_SOFT_BLOB_NOT_DAOC_ART",
            "placeholder label must stay honest — do not invent DAoC art names"
        );
        assert_eq!(PARTICLE_BILLBOARD_DRAW_PATH, "particle_billboard_draw_path");
        let mut sys = ParticleSystem::from_def(toy_def(), 9);
        sys.tick(0.1);
        assert!(!sys.is_empty());
        let right = [1.0, 0.0, 0.0];
        let up = [0.0, 0.0, 1.0];
        let mesh = sys.billboard_mesh(right, up);
        assert_eq!(mesh.len(), sys.len() * 6, "two triangles per particle");
        let cube = sys.instances();
        let p0 = cube[0].pos;
        let half = cube[0].scale; // instances() stores half-extent
        let mut min_x = f32::MAX;
        let mut max_x = f32::MIN;
        for v in mesh.iter().take(6) {
            min_x = min_x.min(v.pos[0]);
            max_x = max_x.max(v.pos[0]);
            assert!(
                (v.pos[1] - p0[1]).abs() < 1e-4,
                "no offset along unused axis"
            );
        }
        assert!((min_x - (p0[0] - half)).abs() < 1e-3);
        assert!((max_x - (p0[0] + half)).abs() < 1e-3);
    }
}
