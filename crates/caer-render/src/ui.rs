//! The **2D overlay layer** — egui hosted over the wgpu viewport, shared by both applications.
//!
//! [`Ui`] owns the egui context + winit input state (the app side); the GPU-side renderer lives in
//! [`crate::gpu::Gpu`] so it shares the device/queue/format. Each frame the app builds its widgets
//! via [`Ui::run`], hands the resulting paint jobs to the GPU, and the GPU draws them after the
//! scene.
//!
//! **`Ui` is deliberately content-agnostic.** [`Ui::run`] takes a closure rather than calling any
//! particular layout, because there are now two very different consumers of the same plumbing:
//! the dev viewer's editor sidebar (below) and the player client's game HUD ([`crate::hud`]).
//! Everything they share — context lifetime, winit event translation, focus handling,
//! tessellation, the screen descriptor — lives here exactly once; everything they don't share
//! lives in the closure. This is the layer C.2–C.7 (quickbar, chat, inventory, map, …) build on,
//! so keeping it free of any one window's assumptions is the whole point.
//!
//! The sidebar half of this file is the viewer's: the readout (zone/FPS/camera), the seam-smoother
//! dial, the fixture editor, the terrain brush, and — next — the asset browser.

use std::sync::Arc;

use winit::window::Window;

/// The switchable "maps" — the multi-zone terrain regions, in menu order. Single-zone regions
/// (dungeons/cities) are omitted; `--region N` still reaches them from the CLI. Names derived from
/// each region's zone names (e.g. region 1 = Camelot Hills… = Albion).
pub const MAP_REGIONS: &[(u16, &str)] = &[
    (1, "Albion"),
    (51, "Albion — Shrouded Isles"),
    (2, "Albion — Housing"),
    (100, "Midgard"),
    (151, "Midgard — Shrouded Isles"),
    (102, "Midgard — Housing"),
    (200, "Hibernia"),
    (181, "Hibernia — Shrouded Isles"),
    (202, "Hibernia — Housing"),
    (163, "New Frontiers (RvR)"),
    (73, "Trials of Atlantis"),
    (30, "Atlantis — Oceanus (Alb)"),
    (130, "Atlantis — Oceanus (Mid)"),
];

fn region_label(region: u16) -> String {
    MAP_REGIONS
        .iter()
        .find(|(id, _)| *id == region)
        .map_or_else(|| format!("Region {region}"), |(_, n)| (*n).to_string())
}

/// Terrain-brush mode. `Flatten` levels to the elevation where the stroke BEGAN, held for the
/// whole drag (start on the keep floor, trace the path, everything drops to floor level);
/// `Smooth` levels toward the mean of the surrounding ground per stamp (the anti-hump tool).
/// Both author the same durable flatten-patch rows in `data/terrain_flatten.tsv`.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum BrushMode {
    #[default]
    Off,
    Flatten,
    Smooth,
    /// Remove the patches centred under the brush and restore the original terrain there.
    Erase,
}

/// State the sidebar reads to render itself. Filled by the app each frame.
#[derive(Default)]
pub struct SidebarState {
    /// Currently loaded region. The Map combo writes the user's pick here; the app compares it to
    /// its own region each frame and reloads when it changes.
    pub region: u16,
    pub zone: String,
    pub fps: u32,
    pub ms_per_frame: f32,
    pub visible: u32,
    pub cam_world: [f32; 3],
    /// Current seam-blend reach (samples). The slider edits `seam_blend_pending`; when it differs
    /// from this, an "Apply" reloads the terrain.
    pub seam_blend: usize,
    pub seam_blend_pending: usize,
    /// Set true by the sidebar when the user clicks "Apply seam blend" — the app reloads terrain.
    pub apply_seam: bool,
    /// Editor status.
    pub edit_mode: bool,
    pub selection: Option<SelectionInfo>,
    /// Terrain brush: mode + stroke parameters (world units). While a mode is active, a left
    /// click in the viewport (edit mode) applies a patch at the terrain hit instead of picking.
    pub brush_mode: BrushMode,
    pub brush_radius: f32,
    pub brush_falloff: f32,
    /// Set true by the sidebar's "Undo last patch" button; the app pops + reloads.
    pub brush_undo: bool,
    /// Patch count in this region (readout) — tells you Undo has something to bite.
    pub brush_patches: usize,
}

pub struct SelectionInfo {
    pub zone_id: u16,
    pub fixture_id: u32,
    pub yaw_deg: f32,
    pub pos: [f32; 3],
}

/// App-side egui: context + winit event translation.
pub struct Ui {
    ctx: egui::Context,
    state: egui_winit::State,
}

impl Ui {
    pub fn new(window: &Arc<Window>) -> Self {
        let ctx = egui::Context::default();
        let state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            Some(2048),
        );
        Self { ctx, state }
    }

    /// Feed a window event to egui. Returns true if egui consumed it (so the app skips camera /
    /// editor handling — e.g. a click that landed on the sidebar).
    pub fn on_event(&mut self, window: &Window, event: &winit::event::WindowEvent) -> bool {
        self.state.on_window_event(window, event).consumed
    }

    /// Build this frame's overlay with `build` and tessellate it. Returns the paint jobs + texture
    /// deltas + screen descriptor the GPU needs to draw it.
    ///
    /// `build` receives the whole-window root `Ui`: a panel carves an edge out of it (the viewer's
    /// sidebar), while an `Area` floats over the scene without reserving space (the HUD).
    pub fn run(&mut self, window: &Window, build: impl FnMut(&mut egui::Ui)) -> EguiFrame {
        let raw_input = self.state.take_egui_input(window);
        // egui 0.35: run_ui hands the closure the root `Ui`; panels/areas attach to it.
        let full = self.ctx.run_ui(raw_input, build);
        self.state
            .handle_platform_output(window, full.platform_output);
        let primitives = self.ctx.tessellate(full.shapes, full.pixels_per_point);
        let size = window.inner_size();
        EguiFrame {
            primitives,
            textures_delta: full.textures_delta,
            screen: egui_wgpu::ScreenDescriptor {
                size_in_pixels: [size.width.max(1), size.height.max(1)],
                pixels_per_point: full.pixels_per_point,
            },
        }
    }

    /// Whether egui currently wants pointer input (hovering the panel) — the app suppresses
    /// camera drags while true so dragging a slider doesn't also spin the world.
    pub fn wants_pointer(&self) -> bool {
        self.ctx.egui_wants_pointer_input()
    }

    /// Whether egui currently wants keyboard input (a text field has focus). Only then should the
    /// app defer keys to egui — otherwise egui's Tab-for-focus-nav would swallow the editor's
    /// Tab / Q-E / WASD hotkeys.
    pub fn wants_keyboard(&self) -> bool {
        self.ctx.egui_wants_keyboard_input()
    }

    /// Drop whatever widget focus egui is holding. Called when a click lands in the 3D viewport:
    /// the Map combo (and friends) otherwise keep keyboard focus after use, which made
    /// `wants_keyboard` stay true forever and swallowed every editor hotkey (the "Tab won't
    /// enter the editor after switching maps" bug).
    pub fn drop_focus(&self) {
        self.ctx.memory_mut(|m| {
            if let Some(id) = m.focused() {
                m.surrender_focus(id);
            }
        });
    }
}

/// Tessellate an overlay with **no window** — the headless counterpart to [`Ui::run`].
///
/// [`Ui`] can't serve the screenshot path because `egui_winit::State` needs a real `Window` to
/// take input from. Headless has no input to take, so this drives a bare `egui::Context` with a
/// synthetic screen rect instead. That is what lets `--screenshot` capture the HUD and what the
/// golden test uses; without it the overlay would be invisible to every automated check.
///
/// Runs the build closure **twice**: egui lays out on the previous frame's sizes, so a first pass
/// can place auto-sized widgets before their size is known (a `CENTER_TOP`-anchored frame lands
/// visibly off-centre). The second pass sees the settled sizes, and only its geometry is kept.
///
/// **Both passes' texture deltas are kept, in order.** egui emits the font atlas upload on the
/// pass that first needs it — the *first* one — so returning only the second pass's deltas hands
/// the renderer geometry that samples a texture it was never given, and the overlay silently
/// renders as nothing. That is not hypothetical: it is exactly how this function was first
/// written, and `hud_golden::the_overlay_reaches_the_framebuffer` caught it.
pub fn tessellate_headless(
    size: [u32; 2],
    pixels_per_point: f32,
    mut build: impl FnMut(&mut egui::Ui),
) -> EguiFrame {
    let ctx = egui::Context::default();
    // Kill animations. egui fades a newly-shown `Area` in over ~0.2s, and a headless pass advances
    // no clock — so every frame would capture the overlay mid-fade, blended toward the background
    // and never reaching its real colours. (This is why the first HUD golden came out muddy: the
    // bars read as (52,37,47) instead of (178,34,34).) Zero animation time also makes the headless
    // output deterministic, which the goldens depend on.
    ctx.all_styles_mut(|s| s.animation_time = 0.0);
    let screen = egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(
            size[0].max(1) as f32 / pixels_per_point,
            size[1].max(1) as f32 / pixels_per_point,
        ),
    );
    let input = || egui::RawInput {
        screen_rect: Some(screen),
        ..Default::default()
    };
    let warmup = ctx.run_ui(input(), &mut build);
    let full = ctx.run_ui(input(), &mut build);
    let primitives = ctx.tessellate(full.shapes, full.pixels_per_point);

    let mut textures_delta = warmup.textures_delta;
    textures_delta.set.extend(full.textures_delta.set);
    textures_delta.free.extend(full.textures_delta.free);

    EguiFrame {
        primitives,
        textures_delta,
        screen: egui_wgpu::ScreenDescriptor {
            size_in_pixels: [size[0].max(1), size[1].max(1)],
            pixels_per_point,
        },
    }
}

/// One frame's worth of tessellated egui geometry, ready for the GPU renderer.
pub struct EguiFrame {
    pub primitives: Vec<egui::ClippedPrimitive>,
    pub textures_delta: egui::TexturesDelta,
    pub screen: egui_wgpu::ScreenDescriptor,
}

/// The actual sidebar layout. `root` is the whole-window `Ui` from `run_ui`; the panel carves the
/// left edge out of it. Pass this to [`Ui::run`] as the build closure.
pub fn build_sidebar(root: &mut egui::Ui, sb: &mut SidebarState) {
    egui::Panel::left("editor_sidebar")
        .resizable(false)
        .default_size(260.0)
        .show(root, |ui| {
            ui.add_space(6.0);
            ui.heading("caer editor");
            ui.separator();

            egui::CollapsingHeader::new("Map")
                .default_open(true)
                .show(ui, |ui| {
                    egui::ComboBox::from_id_salt("map_region")
                        .selected_text(region_label(sb.region))
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            for (id, name) in MAP_REGIONS {
                                ui.selectable_value(&mut sb.region, *id, *name);
                            }
                        });
                    ui.label(egui::RichText::new("switching reloads terrain (a moment)").weak());
                });

            egui::CollapsingHeader::new("Location")
                .default_open(true)
                .show(ui, |ui| {
                    ui.label(egui::RichText::new(&sb.zone).strong());
                    ui.label(format!(
                        "cam  {:.0}, {:.0}, {:.0}",
                        sb.cam_world[0], sb.cam_world[1], sb.cam_world[2]
                    ));
                    ui.label(format!("{} fps · {:.2} ms/frame", sb.fps, sb.ms_per_frame));
                    ui.label(format!("{} visible", sb.visible));
                });

            egui::CollapsingHeader::new("Seam smoothing")
                .default_open(true)
                .show(ui, |ui| {
                    ui.label("blend reach (samples)");
                    ui.add(egui::Slider::new(&mut sb.seam_blend_pending, 0..=24));
                    let dirty = sb.seam_blend_pending != sb.seam_blend;
                    ui.add_enabled_ui(dirty, |ui| {
                        if ui.button("Apply (reload terrain)").clicked() {
                            sb.apply_seam = true;
                        }
                    });
                    if dirty {
                        ui.label(egui::RichText::new("pending — click Apply").weak());
                    }
                });

            egui::CollapsingHeader::new("Fixture editor")
                .default_open(true)
                .show(ui, |ui| {
                    ui.label(if sb.edit_mode {
                        "EDIT mode (Tab to exit)"
                    } else {
                        "fly mode (Tab to edit)"
                    });
                    match &sb.selection {
                        Some(s) => {
                            ui.label(format!("zone {} · fixture {}", s.zone_id, s.fixture_id));
                            ui.label(format!("yaw {:.0}°", s.yaw_deg));
                            ui.label(format!(
                                "pos {:.0}, {:.0}, {:.0}",
                                s.pos[0], s.pos[1], s.pos[2]
                            ));
                            ui.label(
                                egui::RichText::new(
                                    "Q/E rotate · WASD move · Space/Z up-down · R revert",
                                )
                                .weak(),
                            );
                        }
                        None => {
                            ui.label(egui::RichText::new("click a model to select").weak());
                        }
                    }
                });

            egui::CollapsingHeader::new("Terrain brush")
                .default_open(true)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut sb.brush_mode, BrushMode::Off, "Off");
                        ui.selectable_value(&mut sb.brush_mode, BrushMode::Smooth, "Smooth");
                        ui.selectable_value(&mut sb.brush_mode, BrushMode::Flatten, "Flatten");
                        ui.selectable_value(&mut sb.brush_mode, BrushMode::Erase, "Erase");
                    });
                    ui.label("radius (world units)");
                    ui.add(
                        egui::Slider::new(&mut sb.brush_radius, 100.0..=6000.0).logarithmic(true),
                    );
                    ui.label("falloff (% of radius)");
                    ui.add(egui::Slider::new(&mut sb.brush_falloff, 0.0..=150.0));
                    match sb.brush_mode {
                        BrushMode::Off => {
                            ui.label(
                                egui::RichText::new(
                                    "pick a mode, then click/drag terrain (edit mode)",
                                )
                                .weak(),
                            );
                        }
                        BrushMode::Smooth => {
                            ui.label(
                                egui::RichText::new(
                                    "drag over bumps/dips — levels toward surrounding ground",
                                )
                                .weak(),
                            );
                        }
                        BrushMode::Flatten => {
                            ui.label(
                                egui::RichText::new(
                                    "click/drag — levels the area to the first-clicked height",
                                )
                                .weak(),
                            );
                        }
                        BrushMode::Erase => {
                            ui.label(
                                egui::RichText::new("click — removes the patches under the brush")
                                    .weak(),
                            );
                        }
                    }
                    ui.horizontal(|ui| {
                        ui.add_enabled_ui(sb.brush_patches > 0, |ui| {
                            if ui.button("Undo last patch").clicked() {
                                sb.brush_undo = true;
                            }
                        });
                        ui.label(
                            egui::RichText::new(format!("{} patch(es) here", sb.brush_patches))
                                .weak(),
                        );
                    });
                    ui.label(
                        egui::RichText::new("Apply (reload terrain) re-snaps fixtures when done")
                            .weak(),
                    );
                });

            ui.separator();
            ui.label(egui::RichText::new("asset browser — coming next").weak());
        });
}
