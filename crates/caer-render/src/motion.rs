//! Entity motion smoothing: what a mob does BETWEEN server updates.
//!
//! The server sends an NPC's position every few hundred milliseconds at best, and we were rendering
//! that packet position directly. So a walking mob stood frozen — playing its walk cycle on the spot
//! because its `speed` was non-zero — and then jumped to wherever the next update put it. That is
//! the "walks in place then teleports" symptom, and it is a rendering artefact rather than anything
//! wrong with the animation or the server.
//!
//! Two stages, and they do different jobs:
//!
//!  * **Prediction** (dead reckoning): the server told us a position, a heading and a speed, so we
//!    can say where the mob *should* be now by walking it forward from that update. This is what
//!    makes motion continuous.
//!  * **Smoothing**: the predicted point jumps whenever a fresh update disagrees with our guess, so
//!    the rendered position eases toward the prediction rather than snapping to it.
//!
//! Prediction alone judders on every correction; smoothing alone still lags a moving mob behind by
//! the whole update interval. Both are needed.

use std::collections::HashMap;

/// How far a server position must differ from our prediction before we give up and SNAP.
///
/// A real teleport (zoning, a GM jump, a stealth reposition) must not be smoothed — sliding a mob
/// across a zone at walking pace looks far worse than a hard cut. Set well above the error a normal
/// update can accumulate: at DAoC's ~191 u/s ceiling and a generous one-second gap, honest drift is
/// a couple of hundred units.
pub const SNAP_DISTANCE: f32 = 2_000.0;

/// Fraction of the remaining error removed per second of smoothing.
///
/// Ours, not the client's. High enough to converge before the next update lands (so error does not
/// accumulate), low enough that a correction reads as a drift rather than a jerk.
pub const CORRECTION_PER_SEC: f32 = 12.0;

/// How long we keep predicting after the last update before deciding the mob has actually stopped.
///
/// Without a cap, an entity whose updates stop arriving (it left our view, or the server dropped it
/// from the update set) keeps marching off in a straight line forever. Its walk animation would keep
/// playing too — a mob calmly walking through a wall into the distance.
pub const MAX_PREDICT_SECONDS: f32 = 1.5;

/// Largest speed (world units/second) we will believe from differencing two updates.
///
/// DAoC's clean ceiling is ~191 u/s. A larger apparent speed means the two samples straddle a
/// teleport or a very late packet, and extrapolating it would fling the entity across the zone.
pub const MAX_BELIEVABLE_SPEED: f32 = 400.0;

/// One entity's motion state, retained across frames.
#[derive(Clone, Copy, Debug)]
struct State {
    /// Last authoritative position from the server.
    server: [f32; 3],
    /// When that arrived, in the render clock's seconds.
    server_at: f32,
    /// Where we are actually drawing it, which lags the prediction slightly.
    rendered: [f32; 3],
    /// MEASURED velocity, from differencing consecutive server positions.
    ///
    /// Deliberately not derived from the wire's heading+speed. A mob pathing round a corner updates
    /// its position continuously but its heading in steps, so heading-based prediction walks it the
    /// wrong way and the correction yanks it back — the "snapping back and forth" Matt reported.
    /// Differencing positions is self-correcting: if the mob turns, the next sample turns with it.
    velocity: [f32; 3],
    last_seen: f32,
}

/// What the tracker resolved for one entity this frame.
#[derive(Clone, Copy, Debug)]
pub struct Motion {
    /// Where to draw it.
    pub pos: [f32; 3],
    /// Speed observed from actual movement, in world units/second.
    ///
    /// Drives the locomotion clip instead of the wire's `speed` field. The wire can report a speed
    /// while the entity is not moving, which is what made mobs "walk in place".
    pub measured_speed: f32,
}

/// Per-entity motion smoothing.
#[derive(Default)]
pub struct MotionTracker {
    states: HashMap<u16, State>,
}

impl MotionTracker {
    /// Where to draw entity `id` this frame.
    ///
    /// `server` is the latest packet position, `now` the render clock, `dt` the frame delta. Call
    /// once per visible entity per frame; the return value is the position to render.
    pub fn resolve(&mut self, id: u16, server: [f32; 3], now: f32, dt: f32) -> Motion {
        let entry = self.states.entry(id).or_insert(State {
            server,
            server_at: now,
            rendered: server,
            velocity: [0.0; 3],
            last_seen: now,
        });
        entry.last_seen = now;

        // A changed packet position means a fresh update: measure velocity from it, then re-anchor.
        let moved = dist2(entry.server, server);
        if moved > 0.01 {
            if moved > SNAP_DISTANCE * SNAP_DISTANCE {
                // A real teleport. Cut, and drop the velocity — the jump says nothing about how
                // fast the entity is travelling, and believing it would fling it onward afterwards.
                entry.rendered = server;
                entry.velocity = [0.0; 3];
            } else {
                let gap = (now - entry.server_at).max(1e-3);
                let v = [
                    (server[0] - entry.server[0]) / gap,
                    (server[1] - entry.server[1]) / gap,
                    (server[2] - entry.server[2]) / gap,
                ];
                let speed = (v[0] * v[0] + v[1] * v[1]).sqrt();
                // Reject an implausible reading rather than extrapolating it across the zone.
                entry.velocity = if speed <= MAX_BELIEVABLE_SPEED {
                    v
                } else {
                    [0.0; 3]
                };
            }
            entry.server = server;
            entry.server_at = now;
        }

        // Predict along the MEASURED velocity, capped so a stale entity does not wander off.
        let elapsed = (now - entry.server_at).clamp(0.0, MAX_PREDICT_SECONDS);
        let predicted = [
            entry.server[0] + entry.velocity[0] * elapsed,
            entry.server[1] + entry.velocity[1] * elapsed,
            entry.server[2] + entry.velocity[2] * elapsed,
        ];

        // Ease toward the prediction. Exponential so the step is frame-rate independent: a fixed
        // per-frame fraction would converge twice as fast at 120fps as at 60.
        let k = 1.0 - (-CORRECTION_PER_SEC * dt.max(0.0)).exp();
        for (r, p) in entry.rendered.iter_mut().zip(predicted.iter()) {
            *r += (*p - *r) * k;
        }
        // Stop claiming motion once prediction has expired: an entity whose updates stopped is
        // standing still, and should idle rather than keep playing its walk cycle.
        let stale = now - entry.server_at > MAX_PREDICT_SECONDS;
        let measured_speed = if stale {
            0.0
        } else {
            (entry.velocity[0] * entry.velocity[0] + entry.velocity[1] * entry.velocity[1]).sqrt()
        };
        Motion {
            pos: entry.rendered,
            measured_speed,
        }
    }

    /// Forget entities not seen since `now - keep`, so the map does not grow with every mob that
    /// has ever wandered into view.
    pub fn prune(&mut self, now: f32, keep: f32) {
        self.states.retain(|_, s| now - s.last_seen <= keep);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.states.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }
}

fn dist2(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (dx, dy, dz) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    dx * dx + dy * dy + dz * dz
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a straight-line walk: the server reports a new position every `interval` seconds.
    /// Returns the tracker so a test can keep querying it.
    fn walk_north(speed: f32, interval: f32, seconds: f32) -> (MotionTracker, f32, [f32; 3]) {
        let mut m = MotionTracker::default();
        let dt = 1.0 / 60.0;
        let mut t = 0.0f32;
        let mut server = [0.0f32, 0.0, 0.0];
        let mut last_update = 0.0f32;
        m.resolve(1, server, 0.0, dt);
        while t < seconds {
            t += dt;
            if t - last_update >= interval {
                server[1] += speed * (t - last_update);
                last_update = t;
            }
            m.resolve(1, server, t, dt);
        }
        (m, t, server)
    }

    /// First sight places the entity exactly where the server says — no easing in from the origin.
    #[test]
    fn first_sight_snaps_to_the_server_position() {
        let mut m = MotionTracker::default();
        assert_eq!(
            m.resolve(1, [100.0, 200.0, 50.0], 0.0, 0.016).pos,
            [100.0, 200.0, 50.0]
        );
    }

    /// The symptom this exists to fix: between sparse updates a walking mob must KEEP MOVING rather
    /// than standing still playing its walk cycle.
    #[test]
    fn a_walking_mob_keeps_moving_between_sparse_updates() {
        // 100 u/s reported only every 500ms — the regime that produced "walk in place then teleport".
        let (mut m, mut t, server) = walk_north(100.0, 0.5, 2.0);
        let dt = 1.0 / 60.0;
        let a = m.resolve(1, server, t, dt).pos;
        // Advance FRAME BY FRAME. Jumping the clock while passing one frame's dt would give the
        // smoothing a single step to cover the whole gap, which measures the test's stepping rather
        // than the tracker's behaviour.
        for _ in 0..12 {
            t += dt;
            m.resolve(1, server, t, dt);
        }
        let b = m.resolve(1, server, t, dt).pos;
        assert!(
            b[1] > a[1] + 5.0,
            "should have kept walking between updates: {a:?} -> {b:?}"
        );
        assert!(a[0].abs() < 2.0, "should not drift sideways, x={}", a[0]);
    }

    /// Velocity is MEASURED from consecutive positions, so it does not need the wire's speed at all.
    #[test]
    fn velocity_is_measured_from_position_not_taken_from_the_wire() {
        let (mut m, t, server) = walk_north(150.0, 0.25, 1.5);
        let got = m.resolve(1, server, t, 0.016).measured_speed;
        assert!((got - 150.0).abs() < 25.0, "measured {got}, expected ~150");
    }

    /// The "snapping back and forth" case: heading disagreeing with actual travel must not matter,
    /// because prediction no longer consults heading. A mob moving EAST is predicted east even
    /// though nothing ever told us its heading.
    #[test]
    fn prediction_follows_actual_travel_not_a_reported_heading() {
        let mut m = MotionTracker::default();
        let dt = 1.0 / 60.0;
        m.resolve(1, [0.0, 0.0, 0.0], 0.0, dt);
        // Two updates 100 units apart along +X over 0.5s = 200 u/s east. Step frames so the
        // rendered position has time to catch up to the prediction.
        let mut t = 0.5;
        m.resolve(1, [100.0, 0.0, 0.0], t, dt);
        for _ in 0..30 {
            t += dt;
            m.resolve(1, [100.0, 0.0, 0.0], t, dt);
        }
        let p = m.resolve(1, [100.0, 0.0, 0.0], t, dt).pos;
        assert!(
            p[0] > 100.0,
            "should be predicted PAST the last update, east: {p:?}"
        );
        assert!(p[1].abs() < 1.0, "must not wander north/south: {p:?}");
    }

    /// A standing mob must not drift, and must report zero measured speed so it idles.
    #[test]
    fn a_standing_mob_does_not_drift_and_reports_no_speed() {
        let mut m = MotionTracker::default();
        let dt = 1.0 / 60.0;
        let mut t = 0.0;
        m.resolve(1, [500.0, 500.0, 0.0], t, dt);
        for _ in 0..120 {
            t += dt;
            m.resolve(1, [500.0, 500.0, 0.0], t, dt);
        }
        let got = m.resolve(1, [500.0, 500.0, 0.0], t, dt);
        assert!(
            (got.pos[0] - 500.0).abs() < 0.01 && (got.pos[1] - 500.0).abs() < 0.01,
            "drifted to {:?}",
            got.pos
        );
        assert_eq!(
            got.measured_speed, 0.0,
            "a stationary mob must not claim a walk speed"
        );
    }

    /// A real teleport CUTS, and must not leave a huge velocity behind that flings the entity on.
    #[test]
    fn a_teleport_snaps_and_does_not_inherit_a_velocity() {
        let mut m = MotionTracker::default();
        let dt = 1.0 / 60.0;
        m.resolve(1, [0.0, 0.0, 0.0], 0.0, dt);
        let p = m.resolve(1, [50_000.0, 50_000.0, 0.0], 0.1, dt);
        assert!(
            (p.pos[0] - 50_000.0).abs() < 1.0,
            "a teleport must not be smoothed: {:?}",
            p.pos
        );
        assert_eq!(p.measured_speed, 0.0, "the jump must not be read as speed");
        // …and it must stay put afterwards rather than continuing at teleport velocity.
        let q = m.resolve(1, [50_000.0, 50_000.0, 0.0], 0.6, dt);
        assert!(
            (q.pos[0] - 50_000.0).abs() < 1.0,
            "flung onward after a teleport: {:?}",
            q.pos
        );
    }

    /// An implausible apparent speed (two samples straddling a late packet) is rejected rather than
    /// extrapolated across the zone.
    #[test]
    fn an_implausible_measured_speed_is_rejected() {
        let mut m = MotionTracker::default();
        let dt = 1.0 / 60.0;
        m.resolve(1, [0.0, 0.0, 0.0], 0.0, dt);
        // 1000 units in 0.1s = 10,000 u/s — inside the teleport threshold but far too fast.
        let got = m.resolve(1, [1000.0, 0.0, 0.0], 0.1, dt);
        assert_eq!(
            got.measured_speed, 0.0,
            "should not believe {} u/s",
            got.measured_speed
        );
    }

    /// A small correction is SMOOTHED, not snapped — the difference between a drift and a visible jerk.
    #[test]
    fn a_small_correction_is_eased_not_snapped() {
        let mut m = MotionTracker::default();
        m.resolve(1, [0.0, 0.0, 0.0], 0.0, 0.016);
        let p = m.resolve(1, [100.0, 0.0, 0.0], 0.016, 0.016).pos;
        assert!(p[0] > 0.0 && p[0] < 100.0, "should ease, not jump: {p:?}");
    }

    /// Smoothing must be frame-rate independent, or motion converges twice as fast at 120fps.
    #[test]
    fn smoothing_does_not_depend_on_frame_rate() {
        let run = |dt: f32, frames: usize| {
            let mut m = MotionTracker::default();
            m.resolve(1, [0.0, 0.0, 0.0], 0.0, dt);
            let mut t = 0.0;
            for _ in 0..frames {
                t += dt;
                m.resolve(1, [1000.0, 0.0, 0.0], t, dt);
            }
            m.resolve(1, [1000.0, 0.0, 0.0], t, dt).pos[0]
        };
        let (slow, fast) = (run(1.0 / 30.0, 15), run(1.0 / 120.0, 60));
        assert!(
            (slow - fast).abs() < 5.0,
            "30fps reached {slow}, 120fps reached {fast}"
        );
    }

    /// Prediction is capped, and once it expires the entity reports NO speed so it idles instead of
    /// walking on the spot forever.
    #[test]
    fn prediction_expires_and_then_the_entity_idles() {
        let mut m = MotionTracker::default();
        let dt = 1.0 / 60.0;
        m.resolve(1, [0.0, 0.0, 0.0], 0.0, dt);
        m.resolve(1, [0.0, 100.0, 0.0], 0.5, dt); // moving north at 200 u/s
        let mut t = 0.5;
        for _ in 0..600 {
            t += dt;
            m.resolve(1, [0.0, 100.0, 0.0], t, dt);
        }
        let got = m.resolve(1, [0.0, 100.0, 0.0], t, dt);
        assert!(
            got.pos[1] <= 100.0 + 200.0 * MAX_PREDICT_SECONDS + 1.0,
            "ran away to y={}",
            got.pos[1]
        );
        assert_eq!(
            got.measured_speed, 0.0,
            "a stale entity must stop claiming to walk"
        );
    }

    /// State for departed entities is reclaimed.
    #[test]
    fn stale_entities_are_pruned() {
        let mut m = MotionTracker::default();
        m.resolve(1, [0.0; 3], 0.0, 0.016);
        m.resolve(2, [0.0; 3], 10.0, 0.016);
        m.prune(10.0, 5.0);
        assert_eq!(m.len(), 1, "the entity last seen at t=0 should be gone");
    }
}
