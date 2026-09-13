//! A free-fly camera for inspecting the world.
//!
//! Two coordinate frames matter here, and keeping them straight is the whole job:
//!
//! * **World space** — DAoC's absolute integer coordinates. Region-1 positions run to the
//!   hundreds of thousands. The camera's authoritative position lives here so we can hand its
//!   XY straight to [`caer_world::WorldState::render_set_par`] as the cull centre.
//! * **Render space** — world space minus a fixed `origin` (chosen once at load, near the
//!   population centroid). Subtracting the origin keeps the numbers the GPU sees small, so f32
//!   depth precision stays good even though absolute world coords would overflow f32's exact
//!   integer range far from the origin.
//!
//! DAoC's axes: +X east, +Y north, +Z up. We render with +Z as the world up vector.

use glam::{Mat4, Vec3};

/// Default vertical field of view, radians. The free-fly camera keeps this.
const FOV_Y: f32 = 60.0_f32 * std::f32::consts::PI / 180.0;

/// The pre-world stage is framed by a **horizontal** 60°, not a vertical one.
///
/// Measured against the Eden bank, three ways that agree: solving the camera with FOV free
/// recovers a vertical 35.71°; reading the 60° as horizontal predicts 35.98° at 16:9; and the
/// apparent-size error a vertical-60° lens produces is exactly the aspect ratio, 1.778, against
/// 1.845 measured from hand-picked correspondences. A vertical 60° at 16:9 is a **91.5° horizontal**
/// lens, which is why every landmark rendered too small and too far away.
///
/// Horizontal rather than a fixed vertical number because the vertical angle depends on aspect —
/// pinning 35.98° would only be right at 16:9 and would re-break the framing at 4:3.
pub const PREWORLD_FOV_X: f32 = 60.0_f32 * std::f32::consts::PI / 180.0;

/// Vertical FOV that yields [`PREWORLD_FOV_X`] horizontally at `aspect`.
#[must_use]
pub fn fov_y_for_horizontal(fov_x: f32, aspect: f32) -> f32 {
    2.0 * ((fov_x * 0.5).tan() / aspect.max(1e-6)).atan()
}
/// Near/far planes, in world units. Far is generous so a high vantage still sees the slice.
const NEAR: f32 = 5.0;
const FAR: f32 = 250_000.0;
/// Base fly speed (world units / second) and the multiplier applied while boost is held.
const BASE_SPEED: f32 = 4_000.0;
const BOOST: f32 = 6.0;
/// Mouse look sensitivity (radians per pixel of motion).
const LOOK_SENS: f32 = 0.0025;

/// Which movement keys are currently held. The app fills this from keyboard events; the camera
/// integrates it each frame.
#[derive(Default, Clone, Copy)]
pub struct Movement {
    pub forward: bool,
    pub back: bool,
    pub left: bool,
    pub right: bool,
    pub up: bool,
    pub down: bool,
    pub boost: bool,
}

pub struct Camera {
    /// Authoritative position in **world** space (DAoC absolute coords).
    pub world_pos: Vec3,
    /// Fixed offset subtracted to reach render space. Set once at construction.
    origin: Vec3,
    /// Look direction, as yaw (around +Z / up) and pitch (up-down), radians.
    yaw: f32,
    pitch: f32,
    aspect: f32,
    /// Far clip plane. Defaults to `FAR`; screenshot plan views sit high enough to need more.
    far: f32,
    /// Fly-speed multiplier, adjusted by mousewheel (exponential, clamped).
    speed_mul: f32,
    /// Vertical FOV, radians. Defaults to [`FOV_Y`]; the pre-world stage overrides it because it
    /// is framed by a horizontal angle instead (see [`PREWORLD_FOV_X`]).
    fov_y: f32,
    /// Set when the caller framed by a horizontal angle, so `set_aspect` can re-derive `fov_y`.
    fov_x: Option<f32>,
}

impl Camera {
    /// Place the camera at `start` (world coords), aimed at `target` (world coords), with
    /// `origin` as the render-space offset. Yaw/pitch are derived so the opening shot faces the
    /// scene regardless of where `start` sits relative to it.
    pub fn new(start: Vec3, target: Vec3, origin: Vec3, aspect: f32) -> Self {
        let dir = (target - start).normalize_or_zero();
        let yaw = dir.y.atan2(dir.x);
        let pitch = dir.z.clamp(-1.0, 1.0).asin();
        Self {
            world_pos: start,
            origin,
            yaw,
            pitch,
            aspect,
            far: FAR,
            speed_mul: 1.0,
            fov_y: FOV_Y,
            fov_x: None,
        }
    }

    /// Frame by a horizontal FOV instead of the default vertical one, deriving the vertical angle
    /// from the current aspect. Re-derives on [`Camera::set_aspect`] so a resize keeps the
    /// horizontal angle fixed rather than silently widening the shot.
    pub fn set_horizontal_fov(&mut self, fov_x: f32) {
        self.fov_x = Some(fov_x);
        self.fov_y = fov_y_for_horizontal(fov_x, self.aspect);
    }

    /// Current vertical FOV in radians.
    #[must_use]
    pub fn fov_y(&self) -> f32 {
        self.fov_y
    }

    /// Push the far clip plane out (never in) — needed when a screenshot vantage sits higher
    /// above the terrain than the interactive default ever flies.
    pub fn ensure_far(&mut self, far: f32) {
        self.far = self.far.max(far);
    }

    /// Current viewport aspect ratio. Exposed so the shell can assert that a resize actually
    /// propagated to the projection (see `shell::Shell::resize`).
    pub fn aspect(&self) -> f32 {
        self.aspect
    }

    pub fn set_aspect(&mut self, aspect: f32) {
        self.aspect = aspect;
        if let Some(fov_x) = self.fov_x {
            self.fov_y = fov_y_for_horizontal(fov_x, aspect);
        }
    }

    /// Point the camera explicitly (degrees). Used by screenshot mode's `--cam`/`--topdown`.
    pub fn set_look_deg(&mut self, yaw_deg: f32, pitch_deg: f32) {
        self.yaw = yaw_deg.to_radians();
        self.pitch = pitch_deg.to_radians().clamp(-1.54, 1.54);
    }

    /// The XY the world model should cull around this frame (world coords, i32).
    pub fn cull_center(&self) -> [i32; 2] {
        [self.world_pos.x as i32, self.world_pos.y as i32]
    }

    /// Unit forward vector from the current yaw/pitch (+Z up).
    fn forward(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        Vec3::new(cp * cy, cp * sy, sp).normalize()
    }

    /// Mousewheel: each notch scales fly speed by 1.25x (up = faster), clamped to 0.05..64x.
    pub fn adjust_speed(&mut self, notches: f32) {
        self.speed_mul = (self.speed_mul * 1.25_f32.powf(notches)).clamp(0.05, 64.0);
    }

    /// Current fly-speed multiplier (for the title-bar readout).
    pub fn speed_mul(&self) -> f32 {
        self.speed_mul
    }

    /// World-space direction that reads as "screen right" through the mirrored view: the
    /// render-frame right vector (mirrored fwd × Z) pulled back to world by the same mirror.
    fn screen_right(&self) -> Vec3 {
        let f = self.forward();
        (Vec3::new(f.x, -f.y, f.z).cross(Vec3::Z) * Vec3::new(1.0, -1.0, 1.0)).normalize_or_zero()
    }

    /// Apply a mouse-look delta (pixels). Pitch is clamped just shy of straight up/down.
    /// Yaw sense: world yaw is mirrored on screen, so += keeps mouse-right = turn-right.
    pub fn look(&mut self, dx: f32, dy: f32) {
        self.yaw += dx * LOOK_SENS;
        self.pitch = (self.pitch - dy * LOOK_SENS).clamp(-1.54, 1.54);
    }

    /// Left-drag pan: slide the camera in its view plane (no yaw/pitch change). Scaled by the
    /// fly-speed multiplier so panning stays useful at any inspection altitude.
    pub fn pan(&mut self, dx: f32, dy: f32) {
        let fwd = self.forward();
        let right = self.screen_right();
        let up = fwd.cross(right).normalize_or_zero();
        let s = 6.0 * self.speed_mul;
        self.world_pos += (-dx * right + dy * up) * s;
    }

    /// Integrate held movement keys over `dt` seconds.
    pub fn update(&mut self, m: Movement, dt: f32) {
        let fwd = self.forward();
        let up = Vec3::Z;
        let right = self.screen_right();
        let mut dir = Vec3::ZERO;
        if m.forward {
            dir += fwd;
        }
        if m.back {
            dir -= fwd;
        }
        if m.right {
            dir += right;
        }
        if m.left {
            dir -= right;
        }
        if m.up {
            dir += up;
        }
        if m.down {
            dir -= up;
        }
        if dir != Vec3::ZERO {
            let speed = BASE_SPEED * self.speed_mul * if m.boost { BOOST } else { 1.0 };
            self.world_pos += dir.normalize() * speed * dt;
        }
    }

    /// Render-space camera right / up for CPU-expanded particle billboards (matches [`Self::view_proj`]).
    #[must_use]
    pub fn billboard_axes(&self) -> (Vec3, Vec3) {
        let f = self.forward();
        let fwd = Vec3::new(f.x, -f.y, f.z);
        let right = fwd.cross(Vec3::Z).normalize_or_zero();
        let up = right.cross(fwd).normalize_or_zero();
        (right, up)
    }

    /// View-projection matrix mapping **render space** to clip space. Render space mirrors
    /// world Y (world +Y = south, render +Y = north so maps read like the atlas): the camera
    /// stays authoritative in world coords and the mirror applies here, to eye + look direction.
    pub fn view_proj(&self) -> Mat4 {
        let d = self.world_pos - self.origin;
        let eye = Vec3::new(d.x, -d.y, d.z);
        let f = self.forward();
        let view = Mat4::look_to_rh(eye, Vec3::new(f.x, -f.y, f.z), Vec3::Z);
        let proj = Mat4::perspective_rh(self.fov_y, self.aspect, NEAR, self.far);
        proj * view
    }

    /// Unproject a cursor position (pixels, origin top-left) into a **render-space** pick ray:
    /// `(origin, unit_direction)`. Used by the editor to select the model under the cursor.
    pub fn screen_ray(&self, px: f32, py: f32, width: f32, height: f32) -> (Vec3, Vec3) {
        // Pixel → NDC. wgpu clip space is x,y ∈ [-1,1] (y up) with z ∈ [0,1].
        let ndc_x = 2.0 * px / width - 1.0;
        let ndc_y = 1.0 - 2.0 * py / height;
        let inv = self.view_proj().inverse();
        let unproject = |z: f32| {
            let p = inv * glam::Vec4::new(ndc_x, ndc_y, z, 1.0);
            p.truncate() / p.w
        };
        let near = unproject(0.0);
        let far = unproject(1.0);
        (near, (far - near).normalize_or_zero())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pre-world lens is a horizontal 60°, and the whole point is that this is NOT the same as
    /// a vertical 60°. Seen red by pointing the assertion at `FOV_Y`: at 16:9 a vertical 60° is a
    /// 91.5° horizontal lens, which is the defect this replaces.
    #[test]
    fn the_preworld_lens_is_sixty_degrees_across_not_sixty_degrees_tall() {
        let aspect = 1920.0 / 1080.0;
        let fov_y = fov_y_for_horizontal(PREWORLD_FOV_X, aspect);
        assert!(
            (fov_y.to_degrees() - 35.98).abs() < 0.05,
            "vertical FOV at 16:9 should be ~35.98°, got {:.2}°",
            fov_y.to_degrees()
        );
        // The known-bad it replaces: reading 60 as vertical yields a 91.5° horizontal lens.
        let bad_fov_x = 2.0 * ((FOV_Y * 0.5).tan() * aspect).atan();
        assert!(
            bad_fov_x.to_degrees() > 91.0,
            "control: a vertical 60° must be a >91° horizontal lens, got {:.2}°",
            bad_fov_x.to_degrees()
        );
        // Landmarks render 1.778x too small under the defect — exactly the aspect ratio.
        let shrink = (FOV_Y * 0.5).tan() / (fov_y * 0.5).tan();
        assert!(
            (shrink - aspect).abs() < 0.01,
            "the defect's apparent-size error should equal the aspect ratio, got {shrink:.3}"
        );
    }

    /// A resize must keep the horizontal angle fixed. Deriving `fov_y` once at construction would
    /// silently widen the shot at 4:3, which is where a hardcoded 35.98° would have re-broken it.
    #[test]
    fn a_horizontal_fov_survives_a_resize() {
        let mut c = Camera::new(
            Vec3::new(0.0, -100.0, 0.0),
            Vec3::ZERO,
            Vec3::ZERO,
            16.0 / 9.0,
        );
        c.set_horizontal_fov(PREWORLD_FOV_X);
        let wide = c.fov_y();
        c.set_aspect(4.0 / 3.0);
        let narrow_screen = c.fov_y();
        assert!(
            narrow_screen > wide,
            "a 4:3 screen needs a TALLER vertical FOV to keep 60° across: {:.2}° vs {:.2}°",
            narrow_screen.to_degrees(),
            wide.to_degrees()
        );
        for aspect in [16.0 / 9.0, 4.0 / 3.0, 21.0 / 9.0, 1.0] {
            c.set_aspect(aspect);
            let back = 2.0 * ((c.fov_y() * 0.5).tan() * aspect).atan();
            assert!(
                (back - PREWORLD_FOV_X).abs() < 1e-5,
                "horizontal FOV drifted at aspect {aspect}: {:.3}°",
                back.to_degrees()
            );
        }
    }

    /// The free-fly camera is untouched: only the pre-world stage had evidence behind it.
    #[test]
    fn the_default_camera_keeps_its_vertical_fov() {
        let c = Camera::new(
            Vec3::new(0.0, -100.0, 0.0),
            Vec3::ZERO,
            Vec3::ZERO,
            16.0 / 9.0,
        );
        assert!((c.fov_y() - FOV_Y).abs() < 1e-6);
    }
}
