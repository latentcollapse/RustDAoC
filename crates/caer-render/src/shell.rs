//! The shared application shell: window + GPU surface + camera lifecycle.
//!
//! Foundation Audit (2026-07-25) step 4. The repo has two winit applications — the `caer-render`
//! dev viewer and the `rustdaoc` player client — and they had each grown their own copy of this
//! boilerplate: window creation, GPU init, resize, redraw.
//!
//! The two handle resize *differently but both correctly*: the viewer keeps a persistent camera and
//! updates its aspect on resize, while the client rebuilds its follow-camera every frame from
//! `Gpu::aspect()` (which derives from the surface config), so it needs no explicit update. That
//! divergence is currently harmless — but it means the same event is handled by two different
//! mechanisms, and only one of them is obvious from reading the handler. Consolidating here makes
//! the correct behaviour shared and unconditional, so a future camera change cannot silently break
//! one app while the other stays fine.
//!
//! **What deliberately does NOT live here:** input semantics and frame content. The viewer is a
//! free-fly camera with a terrain editor; the client is a third-person orbit camera that syncs
//! movement to the wire. Those differ for real product reasons, and unifying them would be forcing
//! a false abstraction rather than removing duplication. Likewise the viewer's dev-only flags
//! (`--topdown`, `--probe`, `--export-heightmap`, the atlas, terrain patches) are correctly
//! viewer-only and are not "missing" from the client.

use std::sync::Arc;

use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use crate::camera::Camera;
use crate::gpu::Gpu;
use crate::gpu_init::GpuInitError;

/// Window, GPU surface and camera, created together on `resumed` and torn down together.
///
/// All three are `Option` because winit creates the window only once the event loop is running;
/// [`Shell::is_ready`] reports whether initialisation has happened yet.
#[derive(Default)]
pub struct Shell {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    camera: Option<Camera>,
    /// Coalesced resize from winit resize storms — applied once per frame / about_to_wait.
    pending_size: Option<(u32, u32)>,
}

impl Shell {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the window/GPU have been created. Both apps guard `resumed` on this, since winit
    /// can deliver `resumed` more than once.
    pub fn is_ready(&self) -> bool {
        self.gpu.is_some()
    }

    /// Create the window and GPU surface. `far` sizes the depth range (both apps pass a cull
    /// radius). Returns the window so the caller can hand it to anything else that needs it
    /// (the viewer builds its egui context from it).
    ///
    /// The camera is set separately via [`Shell::set_camera`] because the two apps construct very
    /// different ones — but once set, resizing keeps its aspect correct for both.
    ///
    /// Failures are typed [`GpuInitError`] (no launch-path panics for adapter/device/surface).
    pub fn init(
        &mut self,
        event_loop: &ActiveEventLoop,
        title: &str,
        far: f32,
    ) -> Result<Arc<Window>, GpuInitError> {
        // DOL / Mythic pregame plates are authored 1024×768 — match that native size so
        // realm/char plates fill the window without letterbox bars (DisplaySettings can override).
        self.init_with_size(event_loop, title, far, 1024, 768)
    }

    /// Like [`Shell::init`], but with an explicit initial inner size.
    pub fn init_with_size(
        &mut self,
        event_loop: &ActiveEventLoop,
        title: &str,
        far: f32,
        width: u32,
        height: u32,
    ) -> Result<Arc<Window>, GpuInitError> {
        self.init_with_size_capture(event_loop, title, far, width, height, false)
    }

    /// Like [`Shell::init_with_size`], with an explicit diagnostic readback capability. The
    /// product client passes `true` only when `--harness-jsonl` was parsed; environment inherited
    /// from a supervisor cannot silently alter an ordinary product swapchain.
    pub fn init_with_size_capture(
        &mut self,
        event_loop: &ActiveEventLoop,
        title: &str,
        far: f32,
        width: u32,
        height: u32,
        harness_capture: bool,
    ) -> Result<Arc<Window>, GpuInitError> {
        let attrs = Window::default_attributes()
            .with_title(title)
            .with_inner_size(winit::dpi::PhysicalSize::new(width.max(1), height.max(1)));
        let window =
            Arc::new(
                event_loop
                    .create_window(attrs)
                    .map_err(|e| GpuInitError::WindowCreate {
                        detail: e.to_string(),
                    })?,
            );
        let gpu = pollster::block_on(Gpu::new_with_capture(window.clone(), far, harness_capture))?;
        self.window = Some(window.clone());
        self.gpu = Some(gpu);
        Ok(window)
    }

    pub fn set_camera(&mut self, camera: Camera) {
        self.camera = Some(camera);
    }

    /// Record a resize event without immediately reconfiguring the surface.
    ///
    /// Interactive shrink/grow delivers many `WindowEvent::Resized` events per gesture; applying
    /// each as a synchronous `surface.configure` + depth recreate wedges the present loop
    /// (observed: KDE "Not Responding" on character select). Coalesce to the latest size and
    /// apply once via [`Shell::apply_pending_resize`]. Zero sizes (minimize) are recorded but
    /// skipped on apply — wgpu forbids 0×0 surfaces.
    pub fn note_resize(&mut self, width: u32, height: u32) {
        self.pending_size = Some((width, height));
    }

    /// Apply the latest coalesced resize, if any. Returns true when the surface was reconfigured.
    pub fn apply_pending_resize(&mut self) -> bool {
        let Some((w, h)) = self.pending_size.take() else {
            return false;
        };
        if w == 0 || h == 0 {
            // Minimized / obscured — keep last valid surface; do not configure 0×0.
            return false;
        }
        self.resize(w, h);
        true
    }

    /// Whether a coalesced resize is waiting to be applied.
    #[must_use]
    pub fn has_pending_resize(&self) -> bool {
        self.pending_size.is_some()
    }

    /// Handle a window resize: updates the GPU surface **and** the camera aspect.
    ///
    /// Updating the camera is a no-op for an app that rebuilds its camera each frame from
    /// `Gpu::aspect()` (the client does), and load-bearing for one that keeps a persistent camera
    /// (the viewer does). Doing both unconditionally means neither app has to remember which it is.
    ///
    /// Prefer [`Shell::note_resize`] + [`Shell::apply_pending_resize`] on the live event path so
    /// resize storms cannot starve present.
    pub fn resize(&mut self, width: u32, height: u32) {
        let (w, h) = (width.max(1), height.max(1));
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.resize(w, h);
        }
        if let Some(camera) = self.camera.as_mut() {
            camera.set_aspect(w as f32 / h as f32);
        }
    }

    /// Ask the compositor for another frame. Both apps call this from `about_to_wait`.
    pub fn request_redraw(&self) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    pub fn set_title(&self, title: &str) {
        if let Some(window) = self.window.as_ref() {
            window.set_title(title);
        }
    }

    pub fn window(&self) -> Option<&Arc<Window>> {
        self.window.as_ref()
    }

    pub fn gpu(&self) -> Option<&Gpu> {
        self.gpu.as_ref()
    }

    pub fn gpu_mut(&mut self) -> Option<&mut Gpu> {
        self.gpu.as_mut()
    }

    pub fn camera(&self) -> Option<&Camera> {
        self.camera.as_ref()
    }

    pub fn camera_mut(&mut self) -> Option<&mut Camera> {
        self.camera.as_mut()
    }

    /// Both at once, for the frame path — which needs to read the camera and drive the GPU in the
    /// same borrow. Splitting this into two calls fights the borrow checker for no benefit.
    pub fn gpu_camera_mut(&mut self) -> Option<(&mut Gpu, &mut Camera)> {
        match (self.gpu.as_mut(), self.camera.as_mut()) {
            (Some(g), Some(c)) => Some((g, c)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_uninitialised_and_ignores_lifecycle_calls() {
        // A Shell before `resumed` must be inert, not panic: winit can deliver resize/redraw
        // events around window creation, and both apps forward them unconditionally.
        let mut shell = Shell::new();
        assert!(!shell.is_ready());
        assert!(shell.window().is_none());
        assert!(shell.gpu().is_none());
        assert!(shell.camera().is_none());
        assert!(shell.gpu_camera_mut().is_none());
        shell.resize(800, 600); // must not panic with nothing created
        shell.request_redraw();
        shell.set_title("ignored");
    }

    #[test]
    fn resize_updates_camera_aspect_even_without_a_gpu() {
        // Guards the persistent-camera case (the dev viewer): a resize must reach the projection
        // even when no GPU exists in the test. Prevents a future app from resizing the surface and
        // silently keeping a stale aspect.
        use glam::Vec3;
        let mut shell = Shell::new();
        shell.set_camera(Camera::new(
            Vec3::new(0.0, -100.0, 100.0),
            Vec3::ZERO,
            Vec3::ZERO,
            1.0,
        ));
        shell.resize(1600, 800);
        let aspect = shell.camera().expect("camera").aspect();
        assert!(
            (aspect - 2.0).abs() < 1e-6,
            "aspect not updated on resize (got {aspect})"
        );
        // Degenerate sizes are clamped rather than producing NaN/inf.
        shell.resize(0, 0);
        assert!(shell.camera().expect("camera").aspect().is_finite());
    }

    #[test]
    fn note_resize_coalesces_to_latest_size() {
        use glam::Vec3;
        let mut shell = Shell::new();
        shell.set_camera(Camera::new(
            Vec3::new(0.0, -100.0, 100.0),
            Vec3::ZERO,
            Vec3::ZERO,
            1.0,
        ));
        // Storm of intermediate sizes — only the last must stick after apply.
        for n in 1..=40 {
            shell.note_resize(800 + n, 600 + n);
        }
        assert!(shell.has_pending_resize());
        assert!(shell.apply_pending_resize());
        assert!(!shell.has_pending_resize());
        let aspect = shell.camera().expect("camera").aspect();
        let expect = 840.0_f32 / 640.0;
        assert!(
            (aspect - expect).abs() < 1e-5,
            "coalesce must apply last size only (got {aspect}, expect {expect})"
        );
        // Second apply with nothing pending is a no-op.
        assert!(!shell.apply_pending_resize());
    }

    #[test]
    fn minimized_zero_size_is_skipped_on_apply() {
        use glam::Vec3;
        let mut shell = Shell::new();
        shell.set_camera(Camera::new(
            Vec3::new(0.0, -100.0, 100.0),
            Vec3::ZERO,
            Vec3::ZERO,
            1.0,
        ));
        shell.resize(1024, 768);
        shell.note_resize(0, 0);
        assert!(!shell.apply_pending_resize(), "0×0 must not reconfigure");
        let aspect = shell.camera().expect("camera").aspect();
        assert!(
            (aspect - (1024.0 / 768.0)).abs() < 1e-5,
            "minimize must preserve last valid aspect"
        );
    }
}
