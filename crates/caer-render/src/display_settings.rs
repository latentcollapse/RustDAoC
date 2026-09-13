//! Authoritative display mode / resolution controller for rustdaoc.
//!
//! One model owns windowed / borderless / exclusive-fullscreen, enumerated monitor modes,
//! persistence, and safe fallback. Render resolution and UI scale are separate axes — UI scale
//! is a config seam only until a later product slice wires it.

use std::fs;
use std::path::{Path, PathBuf};

use winit::monitor::{MonitorHandle, VideoModeHandle};
use winit::window::{Fullscreen, Window};

/// How the client window occupies the display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMode {
    Windowed,
    Borderless,
    Exclusive,
}

impl WindowMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Windowed => "windowed",
            Self::Borderless => "borderless",
            Self::Exclusive => "exclusive",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "windowed" | "window" => Some(Self::Windowed),
            "borderless" | "borderless-windowed" | "borderless_windowed" => Some(Self::Borderless),
            "exclusive" | "fullscreen" | "exclusive-fullscreen" => Some(Self::Exclusive),
            _ => None,
        }
    }
}

/// A monitor video mode the player can pick (or that we fall back to).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    /// Refresh rate in millihertz (e.g. 60000 = 60 Hz). `0` = unspecified / current.
    pub refresh_millihertz: u32,
}

impl DisplayMode {
    #[must_use]
    pub fn label(self) -> String {
        if self.refresh_millihertz == 0 {
            format!("{}x{}", self.width, self.height)
        } else {
            format!(
                "{}x{} @ {:.0}Hz",
                self.width,
                self.height,
                self.refresh_millihertz as f32 / 1000.0
            )
        }
    }
}

/// Where Windowed lands when it is leaving fullscreen and the player has no windowed size of their
/// own. 1024x768 is the pre-world plate's authored design resolution, so the 2D UI maps 1:1 there
/// (Matt, 2026-08-19). Deliberately not `mode_size`, which is the resolution pick exclusive
/// fullscreen reads — borrowing it would drop a 1920x1080 exclusive mode to 1024x768.
pub const WINDOWED_FALLBACK: DisplayMode = DisplayMode {
    width: 1024,
    height: 768,
    refresh_millihertz: 0,
};

/// Persisted / runtime display preferences. Single source of truth — no duplicate mode logic.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplaySettings {
    pub mode: WindowMode,
    /// When true, windowed size tracks the OS window (dynamic); explicit `mode_size` is ignored
    /// for windowed applies until the player picks a fixed resolution.
    pub use_current_window: bool,
    pub mode_size: DisplayMode,
    /// Internal render scale relative to the surface (1.0 = native). Separate from window size.
    pub render_scale: f32,
    /// Future WoW-style UI scale seam — not applied to layout yet.
    pub ui_scale: f32,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            // Product default (Matt 2026-08-14): borderless fullscreen windowed.
            mode: WindowMode::Borderless,
            // Borderless follows the primary monitor; mode_size is the windowed/exclusive pick.
            use_current_window: true,
            mode_size: DisplayMode {
                width: 1920,
                height: 1080,
                refresh_millihertz: 0,
            },
            render_scale: 1.0,
            ui_scale: 1.0,
        }
    }
}

/// Pure description of what [`apply`] will do to a window. Testable without winit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullscreenIntent {
    None,
    Borderless,
    Exclusive,
}

/// `inner_size: None` means "leave the window where the player put it" — borderless and
/// exclusive take the monitor, and dynamic windowed follows a manual resize. `Some` is an
/// explicit size request. Plate scaling is a separate axis — this intent never mentions
/// letterbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowIntent {
    pub fullscreen: FullscreenIntent,
    pub inner_size: Option<(u32, u32)>,
}

impl DisplaySettings {
    /// Translate settings into a compositor-independent window request.
    ///
    /// `use_current_window` is the windowed-only "Dynamic window size" control. It picks which
    /// size Windowed asks for, not whether it asks at all: a player who wants something other than
    /// the design resolution turns it off and picks from the resolution list.
    ///
    /// Windowed always carries a size (Matt, 2026-08-21). An earlier version only requested one
    /// when leaving fullscreen, to avoid resizing a window the player had dragged — but settings
    /// are only applied on Apply, never on a drag, so the guard protected nothing and left
    /// Windowed sitting at whatever size it already had. Selecting Windowed from a monitor-sized
    /// window therefore did what Full-screen Windowed does.
    #[must_use]
    pub fn window_intent(&self) -> WindowIntent {
        match self.mode {
            WindowMode::Windowed => WindowIntent {
                fullscreen: FullscreenIntent::None,
                inner_size: Some(if self.use_current_window {
                    (WINDOWED_FALLBACK.width, WINDOWED_FALLBACK.height)
                } else {
                    (self.mode_size.width.max(1), self.mode_size.height.max(1))
                }),
            },
            WindowMode::Borderless => WindowIntent {
                fullscreen: FullscreenIntent::Borderless,
                inner_size: None,
            },
            WindowMode::Exclusive => WindowIntent {
                fullscreen: FullscreenIntent::Exclusive,
                inner_size: None,
            },
        }
    }

    #[must_use]
    pub fn config_path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("caer").join("display.cfg")
    }

    pub fn load_or_default() -> Self {
        Self::load_from(&Self::config_path()).unwrap_or_default()
    }

    pub fn load_from(path: &Path) -> Option<Self> {
        let text = fs::read_to_string(path).ok()?;
        Some(Self::parse(&text))
    }

    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for raw in text.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "mode" => {
                    if let Some(m) = WindowMode::parse(v) {
                        s.mode = m;
                    }
                }
                "use_current_window" => s.use_current_window = parse_bool(v),
                "width" => {
                    if let Ok(w) = v.parse() {
                        s.mode_size.width = w;
                    }
                }
                "height" => {
                    if let Ok(h) = v.parse() {
                        s.mode_size.height = h;
                    }
                }
                "refresh_millihertz" | "refresh_mhz" => {
                    if let Ok(r) = v.parse() {
                        s.mode_size.refresh_millihertz = r;
                    }
                }
                "render_scale" => {
                    if let Ok(r) = v.parse::<f32>() {
                        s.render_scale = r.clamp(0.25, 4.0);
                    }
                }
                "ui_scale" => {
                    if let Ok(r) = v.parse::<f32>() {
                        s.ui_scale = r.clamp(0.5, 3.0);
                    }
                }
                _ => {}
            }
        }
        s
    }

    #[must_use]
    pub fn serialize(&self) -> String {
        format!(
            "# CAER display settings — render_scale and ui_scale are separate axes\n\
             mode={}\n\
             use_current_window={}\n\
             width={}\n\
             height={}\n\
             refresh_millihertz={}\n\
             render_scale={:.3}\n\
             ui_scale={:.3}\n",
            self.mode.as_str(),
            if self.use_current_window {
                "true"
            } else {
                "false"
            },
            self.mode_size.width,
            self.mode_size.height,
            self.mode_size.refresh_millihertz,
            self.render_scale,
            self.ui_scale,
        )
    }

    pub fn save(&self) -> Result<(), String> {
        self.save_to(&Self::config_path())
    }

    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        fs::write(path, self.serialize()).map_err(|e| format!("{}: {e}", path.display()))
    }
}

fn parse_bool(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Enumerate unique video modes for a monitor (sorted width, height, refresh).
#[must_use]
pub fn enumerate_modes(monitor: &MonitorHandle) -> Vec<DisplayMode> {
    let mut out: Vec<DisplayMode> = monitor
        .video_modes()
        .map(|vm| {
            let size = vm.size();
            DisplayMode {
                width: size.width,
                height: size.height,
                refresh_millihertz: vm.refresh_rate_millihertz(),
            }
        })
        .collect();
    out.sort_by_key(|m| (m.width, m.height, m.refresh_millihertz));
    out.dedup();
    out
}

/// Pick a video mode handle matching `want`, or the closest available. `None` if the monitor
/// exposes no modes (caller must fall back to borderless/windowed).
pub fn find_video_mode(monitor: &MonitorHandle, want: DisplayMode) -> Option<VideoModeHandle> {
    let modes: Vec<VideoModeHandle> = monitor.video_modes().collect();
    if modes.is_empty() {
        return None;
    }
    modes
        .iter()
        .find(|vm| {
            let s = vm.size();
            s.width == want.width
                && s.height == want.height
                && (want.refresh_millihertz == 0
                    || vm.refresh_rate_millihertz() == want.refresh_millihertz)
        })
        .cloned()
        .or_else(|| {
            modes
                .iter()
                .find(|vm| {
                    let s = vm.size();
                    s.width == want.width && s.height == want.height
                })
                .cloned()
        })
        .or_else(|| modes.into_iter().next())
}

/// Result of applying settings — success, or reverted with a reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyResult {
    Applied,
    Reverted { reason: String },
}

/// One-frame intent emitted by the player-facing display panel. The caller owns applying and
/// persisting settings because only the application owns the winit window lifecycle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DisplayPanelAction {
    pub apply: bool,
    pub close: bool,
}

/// Draw the modern display controls beside DAoC's stock `options_window` skin.
///
/// This deliberately edits a caller-owned draft: changing a combo box must not resize the live
/// surface until the player presses Apply. The stock XML remains the visual/options oracle while
/// this small panel supplies modes that the 2004 UI could not express (borderless and dynamic
/// window sizing).
pub fn draw_player_panel(
    root: &mut egui::Ui,
    draft: &mut DisplaySettings,
    available: &[DisplayMode],
) -> DisplayPanelAction {
    let mut action = DisplayPanelAction::default();
    egui::Window::new("Display")
        .id(egui::Id::new("caer_player_display_settings"))
        .default_pos(egui::pos2(500.0, 80.0))
        .default_width(390.0)
        .resizable(false)
        .collapsible(false)
        .show(root.ctx(), |ui| {
            ui.label("Window mode");
            ui.horizontal_wrapped(|ui| {
                ui.radio_value(&mut draft.mode, WindowMode::Windowed, "Windowed");
                ui.radio_value(
                    &mut draft.mode,
                    WindowMode::Borderless,
                    "Borderless windowed",
                );
                ui.radio_value(
                    &mut draft.mode,
                    WindowMode::Exclusive,
                    "Exclusive fullscreen",
                );
            });

            ui.separator();
            let fixed_size_relevant = draft.mode != WindowMode::Borderless;
            ui.add_enabled_ui(fixed_size_relevant, |ui| {
                if draft.mode == WindowMode::Windowed {
                    ui.checkbox(&mut draft.use_current_window, "Dynamic window size");
                    ui.label(
                        egui::RichText::new(
                            "Dynamic follows manual resize; fixed applies the selected size.",
                        )
                        .weak(),
                    );
                }

                let resolution_enabled =
                    draft.mode == WindowMode::Exclusive || !draft.use_current_window;
                ui.add_enabled_ui(resolution_enabled, |ui| {
                    egui::ComboBox::from_id_salt("caer_display_resolution")
                        .selected_text(draft.mode_size.label())
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            for mode in available {
                                ui.selectable_value(
                                    &mut draft.mode_size,
                                    *mode,
                                    mode.label(),
                                );
                            }
                        });
                });
            });
            if draft.mode == WindowMode::Borderless {
                ui.label(egui::RichText::new("Uses the current monitor's desktop mode.").weak());
            }

            ui.separator();
            ui.label(
                egui::RichText::new(
                    "UI scale is preserved as a separate setting; stock-window scaling lands in refinement.",
                )
                .weak(),
            );
            ui.horizontal(|ui| {
                if ui.button("Defaults").clicked() {
                    *draft = DisplaySettings::default();
                }
                if ui.button("Apply").clicked() {
                    action.apply = true;
                }
                if ui.button("Done").clicked() {
                    action.close = true;
                }
            });
        });
    action
}

/// Apply `settings` to `window`. On exclusive failure, reverts to windowed and returns
/// [`ApplyResult::Reverted`]. Does not persist — caller saves after a successful apply.
///
/// **What is tested and what is not.** [`DisplaySettings::window_intent`] is pure and covered:
/// which fullscreen state each mode asks for, and whether it carries a size. This function is
/// the part that talks to a compositor, and none of it is covered by a test, because the result
/// is the compositor's to decide:
///
/// * `request_inner_size` is a *request*. A tiling compositor, a maximised window, or a size
///   past the monitor may all produce something else, and the value that matters arrives later
///   as a `Resized` event rather than from this call.
/// * Exclusive fullscreen is a wayland-protocols extension that not every compositor implements;
///   [`find_video_mode`] returning a handle is not a promise the mode will be taken.
/// * Leaving fullscreen can recreate the native surface, which invalidates the swapchain
///   configured against the old one.
///
/// So the honest evidence for a mode change is a live run on a real compositor, reading the
/// `resulting_fullscreen` / `inner` line rustdaoc logs after the apply. A green unit test says
/// only that the right request was made.
pub fn apply(window: &Window, settings: &DisplaySettings) -> ApplyResult {
    let previous = snapshot_window(window);
    let intent = settings.window_intent();
    match intent.fullscreen {
        FullscreenIntent::None => {
            // `set_fullscreen(None)` is not a harmless setter on every compositor. In
            // particular, Wayland may tear down and recreate the native surface even when the
            // window is already windowed. Keep that transition off unless we are actually leaving
            // fullscreen.
            if window.fullscreen().is_some() {
                window.set_fullscreen(None);
            }
            if let Some((w, h)) = intent.inner_size {
                let requested = winit::dpi::PhysicalSize::new(w, h);
                if window.inner_size() != requested {
                    let _ = window.request_inner_size(requested);
                }
            }
            ApplyResult::Applied
        }
        FullscreenIntent::Borderless => {
            let monitor = window
                .current_monitor()
                .or_else(|| window.primary_monitor());
            window.set_fullscreen(Some(Fullscreen::Borderless(monitor)));
            ApplyResult::Applied
        }
        FullscreenIntent::Exclusive => {
            let Some(monitor) = window
                .current_monitor()
                .or_else(|| window.primary_monitor())
            else {
                restore_window(window, &previous);
                return ApplyResult::Reverted {
                    reason: "no monitor available for exclusive fullscreen".into(),
                };
            };
            let Some(vm) = find_video_mode(&monitor, settings.mode_size) else {
                restore_window(window, &previous);
                return ApplyResult::Reverted {
                    reason: format!(
                        "no video mode for {} — reverted to windowed",
                        settings.mode_size.label()
                    ),
                };
            };
            window.set_fullscreen(Some(Fullscreen::Exclusive(vm)));
            // If the platform silently rejects exclusive, fall back.
            if window.fullscreen().is_none() {
                restore_window(window, &previous);
                return ApplyResult::Reverted {
                    reason: "exclusive fullscreen unsupported — reverted".into(),
                };
            }
            ApplyResult::Applied
        }
    }
}

#[derive(Clone)]
struct WindowSnapshot {
    fullscreen: Option<Fullscreen>,
    size: winit::dpi::PhysicalSize<u32>,
}

fn snapshot_window(window: &Window) -> WindowSnapshot {
    WindowSnapshot {
        fullscreen: window.fullscreen(),
        size: window.inner_size(),
    }
}

fn restore_window(window: &Window, snap: &WindowSnapshot) {
    window.set_fullscreen(snap.fullscreen.clone());
    if snap.fullscreen.is_none() {
        let _ = window.request_inner_size(snap.size);
    }
}

/// Validate a preferred mode against enumerated modes; returns a safe substitute when missing.
#[must_use]
pub fn sanitize_mode(preferred: DisplayMode, available: &[DisplayMode]) -> DisplayMode {
    if available.is_empty() {
        return preferred;
    }
    if available.iter().any(|m| {
        m.width == preferred.width
            && m.height == preferred.height
            && (preferred.refresh_millihertz == 0
                || m.refresh_millihertz == preferred.refresh_millihertz)
    }) {
        return preferred;
    }
    available
        .iter()
        .copied()
        .find(|m| m.width == preferred.width && m.height == preferred.height)
        .or_else(|| {
            available
                .iter()
                .copied()
                .find(|m| m.width == 1920 && m.height == 1080)
        })
        .unwrap_or(available[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_serialize_parse() {
        let s = DisplaySettings {
            mode: WindowMode::Borderless,
            use_current_window: false,
            mode_size: DisplayMode {
                width: 1920,
                height: 1080,
                refresh_millihertz: 60000,
            },
            render_scale: 1.0,
            ui_scale: 1.25,
        };
        let parsed = DisplaySettings::parse(&s.serialize());
        assert_eq!(parsed.mode, WindowMode::Borderless);
        assert!(!parsed.use_current_window);
        assert_eq!(parsed.mode_size.width, 1920);
        assert_eq!(parsed.mode_size.height, 1080);
        assert_eq!(parsed.mode_size.refresh_millihertz, 60000);
        assert!((parsed.ui_scale - 1.25).abs() < 1e-3);
        assert!((parsed.render_scale - 1.0).abs() < 1e-3);
    }

    #[test]
    fn sanitize_falls_back_when_mode_missing() {
        let available = vec![
            DisplayMode {
                width: 1280,
                height: 720,
                refresh_millihertz: 60000,
            },
            DisplayMode {
                width: 1920,
                height: 1080,
                refresh_millihertz: 60000,
            },
        ];
        let bad = DisplayMode {
            width: 9999,
            height: 9999,
            refresh_millihertz: 0,
        };
        let got = sanitize_mode(bad, &available);
        assert_eq!((got.width, got.height), (1920, 1080));
    }

    #[test]
    fn ui_scale_and_render_scale_are_independent_fields() {
        let s = DisplaySettings {
            ui_scale: 2.0,
            render_scale: 0.5,
            ..Default::default()
        };
        let t = DisplaySettings::parse(&s.serialize());
        assert!((t.ui_scale - 2.0).abs() < 1e-3);
        assert!((t.render_scale - 0.5).abs() < 1e-3);
        assert_ne!(t.ui_scale, t.render_scale);
    }

    #[test]
    fn persist_round_trip_file() {
        let dir = std::env::temp_dir().join(format!(
            "caer-display-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("display.cfg");
        let s = DisplaySettings {
            mode: WindowMode::Exclusive,
            use_current_window: false,
            mode_size: DisplayMode {
                width: 1920,
                height: 1080,
                refresh_millihertz: 144000,
            },
            render_scale: 1.0,
            ui_scale: 1.0,
        };
        s.save_to(&path).unwrap();
        let loaded = DisplaySettings::load_from(&path).unwrap();
        assert_eq!(loaded.mode, WindowMode::Exclusive);
        assert_eq!(loaded.mode_size.width, 1920);
        assert_eq!(loaded.mode_size.refresh_millihertz, 144000);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn fixed_windowed_intent_clears_fullscreen_and_requests_a_finite_size() {
        let s = DisplaySettings {
            mode: WindowMode::Windowed,
            use_current_window: false,
            mode_size: DisplayMode {
                width: 1280,
                height: 720,
                refresh_millihertz: 0,
            },
            ..DisplaySettings::default()
        };
        let intent = s.window_intent();
        assert_eq!(intent.fullscreen, FullscreenIntent::None);
        assert_eq!(intent.inner_size, Some((1280, 720)));
    }

    /// "Dynamic window size" is checked and the resolution list is greyed out. Re-applying
    /// windowed must then leave the window alone — otherwise the checkbox is decoration.
    ///
    /// This is the *not* leaving fullscreen half; the other half is
    /// [`leaving_fullscreen_lands_windowed_on_a_window_sized_window`].
    #[test]
    fn dynamic_windowed_intent_requests_the_design_resolution() {
        let s = DisplaySettings {
            mode: WindowMode::Windowed,
            use_current_window: true,
            mode_size: DisplayMode {
                width: 1280,
                height: 720,
                refresh_millihertz: 0,
            },
            ..DisplaySettings::default()
        };
        let intent = s.window_intent();
        assert_eq!(intent.fullscreen, FullscreenIntent::None);
        // The greyed-out resolution list is still not applied — 1280x720 must not appear. What
        // dynamic windowed requests is the design resolution, not nothing: asserting `None` here
        // is what let Windowed keep a monitor-sized window and behave as Full-screen Windowed.
        assert_eq!(
            intent.inner_size,
            Some((WINDOWED_FALLBACK.width, WINDOWED_FALLBACK.height)),
            "dynamic windowed lands on the design resolution, not the greyed-out pick"
        );
    }

    /// Toggling the checkbox is the only difference between these two, and it has to be visible
    /// in the intent — a mode-only comparison would call them equal.
    #[test]
    fn dynamic_and_fixed_windowed_intents_differ() {
        let fixed = DisplaySettings {
            mode: WindowMode::Windowed,
            use_current_window: false,
            ..DisplaySettings::default()
        };
        let dynamic = DisplaySettings {
            use_current_window: true,
            ..fixed.clone()
        };
        assert_ne!(fixed.window_intent(), dynamic.window_intent());
    }

    /// The checkbox is windowed-only. Borderless and exclusive take the monitor either way.
    #[test]
    fn dynamic_flag_does_not_leak_into_fullscreen_modes() {
        for mode in [WindowMode::Borderless, WindowMode::Exclusive] {
            let on = DisplaySettings {
                mode,
                use_current_window: true,
                ..DisplaySettings::default()
            };
            let off = DisplaySettings {
                use_current_window: false,
                ..on.clone()
            };
            assert_eq!(on.window_intent(), off.window_intent(), "{mode:?}");
            assert_eq!(on.window_intent().inner_size, None, "{mode:?}");
        }
    }

    #[test]
    fn borderless_intent_uses_the_monitor_and_does_not_resize() {
        let s = DisplaySettings {
            mode: WindowMode::Borderless,
            mode_size: DisplayMode {
                width: 800,
                height: 600,
                refresh_millihertz: 0,
            },
            ..DisplaySettings::default()
        };
        let intent = s.window_intent();
        assert_eq!(intent.fullscreen, FullscreenIntent::Borderless);
        assert_eq!(intent.inner_size, None);
    }

    /// Matt, 2026-08-19: "Windowed and Fullscreen Windowed currently do the exact same thing."
    ///
    /// They did. Coming out of a fullscreen mode the window is monitor-sized, and dynamic windowed
    /// asked for no size, so it kept it — the two modes differed only by a border. The known-bad
    /// arm is asserted here rather than described, because the defect *is* an intent that carries
    /// no size, and a test that only checked the fixed-resolution path stayed green throughout.
    #[test]
    fn windowed_always_lands_on_a_window_sized_window() {
        let dynamic = DisplaySettings {
            mode: WindowMode::Windowed,
            use_current_window: true,
            ..DisplaySettings::default()
        };
        // Unconditional, and that is the point. The previous version of this test asserted the
        // *opposite* for the not-leaving-fullscreen case ("a window the player dragged is not
        // resized out from under them") and was green for as long as the defect existed: picking
        // Windowed from a monitor-sized window left it monitor-sized, which is Full-screen
        // Windowed's job. Settings apply on Apply, never on a drag, so there was no drag to
        // protect.
        assert_eq!(
            dynamic.window_intent().inner_size,
            Some((WINDOWED_FALLBACK.width, WINDOWED_FALLBACK.height)),
            "Windowed must request the design resolution however it was reached"
        );

        // The player's own resolution pick outranks the fallback.
        let fixed = DisplaySettings {
            use_current_window: false,
            mode_size: DisplayMode {
                width: 1600,
                height: 900,
                refresh_millihertz: 0,
            },
            ..dynamic.clone()
        };
        assert_eq!(
            fixed.window_intent().inner_size,
            Some((1600, 900)),
            "an explicit resolution outranks the fallback"
        );

        // Exclusive keeps its own resolution: the fallback must not leak across modes.
        let exclusive = DisplaySettings {
            mode: WindowMode::Exclusive,
            ..fixed.clone()
        };
        assert_eq!(exclusive.window_intent().inner_size, None);
    }

    #[test]
    fn exclusive_intent_does_not_request_inner_size() {
        let s = DisplaySettings {
            mode: WindowMode::Exclusive,
            ..DisplaySettings::default()
        };
        let intent = s.window_intent();
        assert_eq!(intent.fullscreen, FullscreenIntent::Exclusive);
        assert_eq!(intent.inner_size, None);
    }
}
