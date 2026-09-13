//! Source-backed camera interaction state for the character customizer.
//!
//! `pregame/character_customize.xml` explicitly authors seven camera buttons and the two
//! instructions "Left click and drag to rotate" / "Right click and drag to zoom".  The scene NIFs
//! do not contain a camera, so this module deliberately owns only interaction state: scene
//! framing remains in [`crate::preworld_scene`], where the immutable realm composition belongs.
//!
//! The exact XML establishes *which* controls exist and where they live.  Its executable handler
//! is not present in the retail asset tree, so the bounded increments below are deliberately
//! explicit and independently testable instead of being smuggled into renderer input code.

/// One retail `character_customize.xml` camera control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CameraControl {
    /// ControlId 1014 / `camera_reset`.
    Reset,
    /// ControlId 1024 / `camera_left`.
    RotateLeft,
    /// ControlId 1025 / `camera_right`.
    RotateRight,
    /// ControlId 1026 / `camera_up`.
    TiltUp,
    /// ControlId 1027 / `camera_down`.
    TiltDown,
    /// ControlId 1029 / `camera_zoom_in`.
    ZoomIn,
    /// ControlId 1030 / `camera_zoom_out`.
    ZoomOut,
}

/// Which fixed composition a local preview controller starts from.
///
/// Character selection deliberately keeps its full-body stage composition.  The live
/// character-customizer, by contrast, opens on a face frame in the reference client.  Carrying
/// this in the controller makes Reset restore the screen's own default rather than accidentally
/// pulling a customizer back to the character-select camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CameraFrame {
    FullBody,
    Face,
}

/// Mutable local view state for Character Customize and Character Stats.
///
/// `yaw` turns the displayed avatar rather than moving the realm scene.  This preserves the
/// source-visible distinction between an authored backdrop and a player-operated preview.  Dolly
/// is a multiplier over the calibrated full-body realm framing; `1.0` is the normal initial view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CustomizerCamera {
    yaw: f32,
    tilt: f32,
    dolly: f32,
    frame: CameraFrame,
}

impl Default for CustomizerCamera {
    fn default() -> Self {
        Self {
            yaw: 0.0,
            tilt: 0.0,
            dolly: 1.0,
            frame: CameraFrame::FullBody,
        }
    }
}

impl CustomizerCamera {
    /// Initial close framing observed when the retail customizer opens.
    ///
    /// This value is intentionally separate from the full-body stage camera.  At the pre-world
    /// 60° horizontal lens it fits the face and upper shoulders while retaining enough of the
    /// realm scene to match the source composition.
    pub const FACE_FRAME_DOLLY: f32 = 0.155;
    /// Lowest safe stage-frame multiplier.  It keeps the lens in front of the avatar and makes a
    /// face inspection possible without moving the realm camera used by character select.
    pub const MIN_DOLLY: f32 = 0.15;
    /// Highest useful pull-back multiplier.  A larger value only wastes the fixed pre-world stage.
    pub const MAX_DOLLY: f32 = 1.60;
    /// The dedicated up/down controls are bounded below vertical, so the retail stage can never
    /// be viewed exactly edge-on through this path.
    pub const MAX_TILT: f32 = 0.60;

    const BUTTON_YAW: f32 = std::f32::consts::PI / 12.0;
    const BUTTON_TILT: f32 = std::f32::consts::PI / 36.0;
    const BUTTON_ZOOM: f32 = 0.85;
    const DRAG_YAW: f32 = 0.007;
    const DRAG_TILT: f32 = 0.004;
    const DRAG_ZOOM: f32 = 0.010;

    /// Start from normal framing but accept a bounded deterministic capture value.
    #[must_use]
    pub fn with_dolly(dolly: f32) -> Self {
        let mut camera = Self::default();
        camera.dolly = Self::clamp_dolly(dolly);
        camera
    }

    /// Start from normal framing but accept bounded deterministic capture values.
    #[must_use]
    pub fn with_view(dolly: f32, tilt: f32, yaw: f32) -> Self {
        let mut camera = Self::with_dolly(dolly);
        camera.tilt = Self::clamp_tilt(tilt);
        camera.yaw = yaw.rem_euclid(std::f32::consts::TAU);
        camera
    }

    /// Start on the source customizer's face/upper-body composition.
    #[must_use]
    pub fn face_default() -> Self {
        Self {
            yaw: 0.0,
            tilt: 0.0,
            dolly: Self::FACE_FRAME_DOLLY,
            frame: CameraFrame::Face,
        }
    }

    /// Face-frame equivalent of [`Self::with_view`], used by deterministic screenshot flags.
    #[must_use]
    pub fn with_face_view(dolly: f32, tilt: f32, yaw: f32) -> Self {
        let mut camera = Self::face_default();
        camera.dolly = Self::clamp_dolly(dolly);
        camera.tilt = Self::clamp_tilt(tilt);
        camera.yaw = yaw.rem_euclid(std::f32::consts::TAU);
        camera
    }

    /// Whether this controller should use the character-customizer close composition.
    #[must_use]
    pub const fn uses_face_frame(self) -> bool {
        matches!(self.frame, CameraFrame::Face)
    }

    #[must_use]
    pub const fn yaw(self) -> f32 {
        self.yaw
    }

    #[must_use]
    pub const fn tilt(self) -> f32 {
        self.tilt
    }

    #[must_use]
    pub const fn dolly(self) -> f32 {
        self.dolly
    }

    /// Apply an authored button, with each result constrained to a safe preview range.
    pub fn apply(&mut self, control: CameraControl) {
        match control {
            CameraControl::Reset => {
                *self = match self.frame {
                    CameraFrame::FullBody => Self::default(),
                    CameraFrame::Face => Self::face_default(),
                }
            }
            CameraControl::RotateLeft => self.yaw -= Self::BUTTON_YAW,
            CameraControl::RotateRight => self.yaw += Self::BUTTON_YAW,
            CameraControl::TiltUp => self.tilt = Self::clamp_tilt(self.tilt + Self::BUTTON_TILT),
            CameraControl::TiltDown => self.tilt = Self::clamp_tilt(self.tilt - Self::BUTTON_TILT),
            CameraControl::ZoomIn => self.dolly = Self::clamp_dolly(self.dolly * Self::BUTTON_ZOOM),
            CameraControl::ZoomOut => {
                self.dolly = Self::clamp_dolly(self.dolly / Self::BUTTON_ZOOM)
            }
        }
        self.yaw = self.yaw.rem_euclid(std::f32::consts::TAU);
    }

    /// Retail's left-drag instruction: spin the avatar and tilt the local preview.
    pub fn drag_rotate(&mut self, dx: f32, dy: f32) {
        if dx.is_finite() {
            self.yaw = (self.yaw + dx * Self::DRAG_YAW).rem_euclid(std::f32::consts::TAU);
        }
        if dy.is_finite() {
            self.tilt = Self::clamp_tilt(self.tilt + dy * Self::DRAG_TILT);
        }
    }

    /// Retail's right-drag instruction: vertical drag changes the preview dolly.
    ///
    /// Upward drag (negative `dy`) zooms in; downward drag pulls back out.
    pub fn drag_zoom(&mut self, dy: f32) {
        if dy.is_finite() {
            self.dolly = Self::clamp_dolly(self.dolly * (dy * Self::DRAG_ZOOM).exp());
        }
    }

    const fn clamp_dolly(value: f32) -> f32 {
        if value.is_finite() {
            value.clamp(Self::MIN_DOLLY, Self::MAX_DOLLY)
        } else {
            1.0
        }
    }

    const fn clamp_tilt(value: f32) -> f32 {
        if value.is_finite() {
            value.clamp(-Self::MAX_TILT, Self::MAX_TILT)
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CameraControl, CustomizerCamera};

    #[test]
    fn reset_round_trips_every_preview_axis() {
        let mut view = CustomizerCamera::default();
        view.apply(CameraControl::RotateRight);
        view.apply(CameraControl::TiltUp);
        view.apply(CameraControl::ZoomIn);
        assert_ne!(view, CustomizerCamera::default());
        view.apply(CameraControl::Reset);
        assert_eq!(view, CustomizerCamera::default());
    }

    #[test]
    fn face_reset_preserves_the_customizer_default_not_the_character_select_frame() {
        let mut view = CustomizerCamera::face_default();
        view.apply(CameraControl::ZoomOut);
        view.apply(CameraControl::TiltUp);
        view.apply(CameraControl::Reset);
        assert_eq!(view, CustomizerCamera::face_default());
        assert!(view.uses_face_frame());
        assert!(view.dolly() < 1.0);
    }

    #[test]
    fn source_camera_controls_are_bounded_and_directional() {
        let mut view = CustomizerCamera::default();
        view.apply(CameraControl::ZoomIn);
        assert!(view.dolly() < 1.0, "zoom in brings the lens closer");
        view.apply(CameraControl::ZoomOut);
        assert!((view.dolly() - 1.0).abs() < f32::EPSILON);

        view.drag_rotate(100_000.0, 100_000.0);
        view.drag_zoom(-100_000.0);
        assert!(view.tilt() <= CustomizerCamera::MAX_TILT);
        assert!(view.dolly() >= CustomizerCamera::MIN_DOLLY);

        // Known-bad target: a downward drag must never pretend to zoom in.
        let close = view.dolly();
        view.drag_zoom(100_000.0);
        assert!(view.dolly() >= close);
        assert!(view.dolly() <= CustomizerCamera::MAX_DOLLY);
    }
}
