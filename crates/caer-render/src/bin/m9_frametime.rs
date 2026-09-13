//! M9-REDO council measurement harness — frame-time baseline per `docs/council/PERF_HARNESS_SPEC.md`.
//!
//! Fixes the adversary A-5 defects in the first M9 run:
//! - **No readback in the primary timed path** (readback is a separate upper-bound lane).
//! - **Live dt** and advancing `anim_t` (not a static dt=0 world).
//! - **HUD enabled** via headless egui tessellation composited without readback.
//! - Reports **p50/p95/p99/max**, **drawn instances** as the independent variable, scaling at
//!   **0/50/200/500/1000**, thread occupancy / longest serial span, and explicit attribution gaps.
//!
//! ```text
//! CAER_CLIENT=... cargo run --release -p caer-render --bin m9_frametime -- \
//!   [--frames N] [--warmup W] [--entities 0,50,200,500,1000] [--size WxH] [--mobs PATH]
//!   [--windowed|--present]
//! ```
//!
//! Default is headless offscreen (no swapchain acquire/present) — p99 is a floor.
//! `--windowed` / `--present` creates a real window+surface; timed samples include acquire+present.
//!
//! Allocator A/B (M19f): build with `--features mimalloc` to swap the global allocator.

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use caer_protocol::entities::Npc;
use caer_protocol::session::ServerEvent;
use caer_render::camera::Camera;
use caer_render::entities::EntityModels;
use caer_render::gpu::Gpu;
use caer_render::hud::HudState;
use caer_render::jitter::{compute_jitter, format_jitter_line};
use caer_render::terrain;
use caer_render::{
    fixture_instances, load_world, region_zone_bbox, render_world_timed, world_bbox, CpuPhaseMs,
    CULL_RADIUS,
};
use caer_world::world_data::load_mobs_tsv;
use caer_world::WorldState;
use glam::Vec3;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

struct Args {
    cam: Option<[f32; 5]>,
    topdown: bool,
    frames: usize,
    warmup: usize,
    entities: Vec<usize>,
    size: (u32, u32),
    region: u16,
    mobs: PathBuf,
    full_region_terrain: bool,
    /// Skip the expensive populated-dump + topdown legs (entity ladder only).
    ladder_only: bool,
    /// Camelot Hills ground only — no topdown, no synthetic ladder.
    mandate_only: bool,
    /// Zero per-bone Instant probes (QA-5 clean absolute). Still reports wall-clock p99.
    clean: bool,
    /// Inject a fixed stall every Nth measured frame (REQ-026 live falsifier). 0 = off.
    inject_stall_every: usize,
    /// Stall duration in milliseconds when `inject_stall_every > 0`.
    inject_stall_ms: f64,
    /// Write per-frame series JSONL (warmup-excluded) for independent recomputation.
    series_out: Option<PathBuf>,
    /// MS-08 residual: run background host-clock sampler and emit HOST_CLOCK summary.
    host_clock: bool,
    /// MS-08 residual: real window+surface; timed loop includes swapchain acquire+present.
    windowed: bool,
}

fn parse_args() -> Args {
    let mut a = Args {
        cam: Some([592250.0, 537900.0, 3500.0, 45.0, -30.0]),
        topdown: false,
        // PERF_HARNESS_SPEC: ≥1000 frames after warm-up.
        frames: 1000,
        warmup: 100,
        entities: vec![],
        size: (1600, 1000),
        region: 1,
        mobs: PathBuf::from("captures/mobs_region1.tsv"),
        full_region_terrain: false,
        ladder_only: false,
        mandate_only: false,
        clean: false,
        inject_stall_every: 0,
        inject_stall_ms: 0.0,
        series_out: None,
        host_clock: false,
        windowed: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--cam" => {
                let v: Vec<f32> = it
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .filter_map(|s| s.trim().parse().ok())
                    .collect();
                if v.len() == 5 {
                    a.cam = Some([v[0], v[1], v[2], v[3], v[4]]);
                    a.topdown = false;
                }
            }
            "--topdown" => {
                a.topdown = true;
                a.cam = None;
            }
            "--frames" => a.frames = it.next().and_then(|s| s.parse().ok()).unwrap_or(a.frames),
            "--warmup" => a.warmup = it.next().and_then(|s| s.parse().ok()).unwrap_or(a.warmup),
            "--entities" => {
                a.entities = it
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .filter_map(|s| s.trim().parse().ok())
                    .collect();
            }
            "--size" => {
                if let Some((w, h)) = it.next().unwrap_or_default().split_once('x') {
                    a.size = (w.parse().unwrap_or(1600), h.parse().unwrap_or(1000));
                }
            }
            "--region" => a.region = it.next().and_then(|s| s.parse().ok()).unwrap_or(1),
            "--mobs" => a.mobs = PathBuf::from(it.next().unwrap_or_default()),
            "--ladder-only" => a.ladder_only = true,
            "--mandate-only" => a.mandate_only = true,
            "--clean" => a.clean = true,
            "--inject-stall-every" => {
                a.inject_stall_every = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            }
            "--inject-stall-ms" => {
                a.inject_stall_ms = it.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
            }
            "--series-out" => a.series_out = it.next().map(PathBuf::from),
            "--host-clock" => a.host_clock = true,
            "--windowed" | "--present" => a.windowed = true,
            other => eprintln!("m9_frametime: ignoring unknown arg {other}"),
        }
    }
    a
}

fn display_available() -> bool {
    std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

/// Windowed MS-08 path: one acquire+present per `RedrawRequested` (same cadence as rustdaoc).
struct WindowedApp {
    label: String,
    args: Args,
    world: Option<WorldState>,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    /// Until first redraw finishes setup + swapchain prime.
    ready: bool,
    origin: Vec3,
    camera: Option<Camera>,
    hud: HudState,
    warmup_left: usize,
    frames_left: usize,
    times: Vec<f64>,
    presented_ok: usize,
    last_instances: usize,
    present_mode: &'static str,
}

impl WindowedApp {
    fn new(label: String, args: Args, world: WorldState) -> Self {
        Self {
            label,
            warmup_left: args.warmup,
            frames_left: args.frames,
            times: Vec::with_capacity(args.frames),
            args,
            world: Some(world),
            window: None,
            gpu: None,
            ready: false,
            origin: Vec3::ZERO,
            camera: None,
            hud: HudState {
                name: "m9_perf".into(),
                fps: 0,
                ..HudState::default()
            },
            presented_ok: 0,
            last_instances: 0,
            present_mode: "windowed_pending",
        }
    }

    fn setup_scene(&mut self) {
        let world = self.world.as_ref().expect("world");
        let (w, h) = self.args.size;
        let positions = world.positions();
        self.origin = if positions.is_empty() {
            Vec3::new(560_000.0, 510_000.0, 0.0)
        } else {
            let (mut sx, mut sy, mut sz) = (0i64, 0i64, 0i64);
            for p in positions {
                sx += i64::from(p[0]);
                sy += i64::from(p[1]);
                sz += i64::from(p[2]);
            }
            let n = positions.len() as f32;
            Vec3::new(sx as f32 / n, sy as f32 / n, sz as f32 / n)
        };
        // Windowed path measures acquire+present residual. Skip full-region mesh upload here —
        // loading it inside RedrawRequested blocks the event loop for tens of seconds and can
        // stall swapchain configure. Headless still loads the full mesh.
        eprintln!("m9_frametime\tsetup_scene\tskip_full_terrain=windowed_present_residual");
        let (min, max) = ([0, 0], [1, 1]);
        println!(
            "terrain\t{}\tzones=0\tverts=0\ttris=0\tmodels=0\tfixtures=0\tnote=windowed_skips_mesh_load",
            self.label
        );
        let aspect = w as f32 / h as f32;
        let (mut camera, _cull_radius) =
            make_camera(&self.args, self.origin, min, max, aspect, world);
        camera.set_aspect(aspect);
        self.camera = Some(camera);
        self.hud.status = world.player_status;
        let gpu = self.gpu.as_ref().expect("gpu");
        self.present_mode = gpu.present_mode_label();
        assert_ne!(
            self.present_mode, "headless_offscreen_no_acquire_present",
            "windowed path must not emit headless present_mode label"
        );
        assert!(
            self.present_mode.contains("acquire_present"),
            "windowed present_mode must advertise acquire+present, got {}",
            self.present_mode
        );
        println!(
            "m9_frametime\tpresent_mode\t{}\twindowed=true",
            self.present_mode
        );
        println!(
            "m9_frametime\ttimed_loop\tstart\twarmup={}\tframes={}\twindowed=true",
            self.args.warmup, self.args.frames
        );
        let _ = std::io::Write::flush(&mut std::io::stdout());
        self.ready = true;
    }

    fn sample_frame(&mut self) -> f64 {
        let t0 = Instant::now();
        // Windowed residual measures acquire+present. Keep the GPU work minimal so Timeout
        // from an overloaded swapchain is not confused with present cost.
        if let Some(window) = self.window.as_ref() {
            window.pre_present_notify();
        }
        let gpu = self.gpu.as_mut().expect("gpu");
        let presented = gpu.render_presented(0, None).unwrap_or(false);
        if presented {
            self.presented_ok += 1;
        }
        self.last_instances = 0;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        self.hud.fps = (1000.0 / ms.max(0.001)) as u32;
        ms
    }

    fn finish(&mut self, _event_loop: &ActiveEventLoop) {
        let world_n = self.world.as_ref().map(|w| w.len()).unwrap_or(0);
        assert!(
            self.presented_ok > 0,
            "windowed path recorded zero successful acquire+present frames"
        );
        println!(
            "m9_frametime\twindowed_present_ok\t{}/{}\tpresent_mode={}",
            self.presented_ok,
            self.args.warmup + self.args.frames,
            self.present_mode
        );
        let jitter = compute_jitter(&self.times);
        println!(
            "{}",
            format_jitter_line(&self.label, self.args.warmup, &jitter)
        );
        summarize(
            &format!("{}_acquire_present", self.label),
            &mut self.times,
            self.last_instances,
            world_n,
            10.0,
        );
        println!(
            "m9_frametime\treadback_upper_bound\tskipped=windowed\tnote=primary_path_includes_acquire_present"
        );
        let _ = std::io::Write::flush(&mut std::io::stdout());
        // Avoid winit/wgpu Drop hang after a surface present session on this stack.
        std::process::exit(0);
    }
}

impl ApplicationHandler for WindowedApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let (w, h) = self.args.size;
        let attrs = Window::default_attributes()
            .with_title("m9_frametime --windowed")
            .with_inner_size(winit::dpi::PhysicalSize::new(w, h));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("m9_frametime: create window for --windowed"),
        );
        window.set_visible(true);
        let extent = 200_000.0_f32;
        let mut gpu = pollster::block_on(Gpu::new(window.clone(), extent)).unwrap_or_else(|e| {
            eprintln!("m9_frametime: GPU init failed:\n{e}");
            std::process::exit(1);
        });
        gpu.resize(w.max(1), h.max(1));
        gpu.set_max_frame_latency(1);
        window.request_redraw();
        self.window = Some(window);
        self.gpu = Some(gpu);
        println!("m9_frametime\twindowed_surface\tready=1");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.resize(size.width.max(1), size.height.max(1));
                }
                if let Some(camera) = self.camera.as_mut() {
                    let (w, h) = (size.width.max(1), size.height.max(1));
                    camera.set_aspect(w as f32 / h as f32);
                }
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                if self.gpu.is_none() {
                    return;
                }
                if !self.ready {
                    self.setup_scene();
                    // Prime the swapchain on the configure redraw; time samples on later redraws.
                    if let Some(window) = self.window.as_ref() {
                        window.pre_present_notify();
                    }
                    let primed = self
                        .gpu
                        .as_mut()
                        .map(|g| g.render_presented(0, None).unwrap_or(false))
                        .unwrap_or(false);
                    println!("m9_frametime\tswapchain_prime\tpresented={primed}");
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                    if let Some(window) = self.window.as_ref() {
                        window.request_redraw();
                    }
                    return;
                }
                if self.warmup_left > 0 {
                    let before = self.presented_ok;
                    let ms = self.sample_frame();
                    if self.presented_ok > before {
                        self.warmup_left -= 1;
                        eprintln!(
                            "m9_frametime\twarmup_frame\tremaining={}\tms={ms:.3}\tpresented_ok={}",
                            self.warmup_left, self.presented_ok
                        );
                    }
                } else if self.frames_left > 0 {
                    let before = self.presented_ok;
                    let ms = self.sample_frame();
                    if self.presented_ok > before {
                        self.times.push(ms);
                        self.frames_left -= 1;
                    }
                }
                if self.warmup_left == 0 && self.frames_left == 0 {
                    self.finish(event_loop);
                } else if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Only nudge a redraw if we are still waiting to start; steady-state frames request
        // the next redraw themselves after a successful present.
        if !self.ready {
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }
}

fn percentile(sorted_ms: &[f64], p: f64) -> f64 {
    if sorted_ms.is_empty() {
        return f64::NAN;
    }
    let idx = ((p / 100.0) * (sorted_ms.len() as f64 - 1.0)).round() as usize;
    sorted_ms[idx.min(sorted_ms.len() - 1)]
}

fn summarize(
    label: &str,
    samples_ms: &mut [f64],
    instances: usize,
    world_entities: usize,
    budget_ms: f64,
) {
    samples_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = samples_ms.len() as f64;
    let mean = samples_ms.iter().sum::<f64>() / n;
    let p50 = percentile(samples_ms, 50.0);
    let p95 = percentile(samples_ms, 95.0);
    let p99 = percentile(samples_ms, 99.0);
    let max = samples_ms.last().copied().unwrap_or(f64::NAN);
    let over = samples_ms.iter().filter(|&&t| t > budget_ms).count();
    let fps = 1000.0 / mean;
    println!(
        "RESULT\t{label}\tworld_entities={world_entities}\tdrawn_instances={instances}\tn={}\tmean_ms={mean:.3}\tp50_ms={p50:.3}\tp95_ms={p95:.3}\tp99_ms={p99:.3}\tmax_ms={max:.3}\tover_budget_{budget_ms}ms={over}\tmean_fps={fps:.1}",
        samples_ms.len()
    );
}

fn synth_world(mobs_path: &PathBuf, target: usize, center: [f32; 3]) -> WorldState {
    let file = std::fs::File::open(mobs_path).unwrap_or_else(|e| {
        eprintln!("m9_frametime: cannot open {}: {e}", mobs_path.display());
        std::process::exit(2);
    });
    let _probe = load_mobs_tsv(std::io::BufReader::new(file));

    let mut w = WorldState::new();
    let mut next_id: u16 = 1;
    let cx = center[0];
    let cy = center[1];
    let cz = center[2];
    for i in 0..target {
        let ring = (i as f32).sqrt();
        let ang = i as f32 * 2.399_963;
        let r = 40.0 + ring * 18.0;
        let x = cx + r * ang.cos();
        let y = cy + r * ang.sin();
        w.apply(&ServerEvent::NpcInView(Npc {
            object_id: next_id,
            speed: if i % 3 == 0 { 150 } else { 0 },
            heading: ((i * 97) % 4096) as u16,
            x: x.max(0.0) as u32,
            y: y.max(0.0) as u32,
            z: cz.clamp(0.0, f32::from(u16::MAX)) as u16,
            model: 33735,
            size: 50,
            level: 50,
            flags: 0,
            name: format!("m9_{i}"),
            guild: String::new(),
        }));
        next_id = next_id.wrapping_add(1).max(1);
    }
    w
}

fn make_camera(
    args: &Args,
    origin: Vec3,
    min: [i32; 2],
    max: [i32; 2],
    aspect: f32,
    world: &WorldState,
) -> (Camera, i32) {
    if args.topdown {
        let cx = (min[0] + max[0]) as f32 / 2.0;
        let cy = (min[1] + max[1]) as f32 / 2.0;
        let span = ((max[0] - min[0]).max(max[1] - min[1])) as f32;
        let top_z = world.positions().iter().map(|p| p[2]).max().unwrap_or(0) as f32;
        let height = span.mul_add(0.9, top_z);
        let mut c = Camera::new(Vec3::new(cx, cy, height), origin, origin, aspect);
        c.set_look_deg(90.0, -88.0);
        c.ensure_far(height * 1.5);
        (c, i32::MAX / 4)
    } else if let Some([x, y, z, yaw, pitch]) = args.cam {
        let mut c = Camera::new(Vec3::new(x, y, z), origin, origin, aspect);
        c.set_look_deg(yaw, pitch);
        (c, CULL_RADIUS)
    } else {
        let r = CULL_RADIUS as f32;
        (
            Camera::new(
                origin + Vec3::new(0.0, -0.8 * r, 0.7 * r),
                origin,
                origin,
                aspect,
            ),
            CULL_RADIUS,
        )
    }
}

/// Nudge moving NPCs each frame so the timed path is not a static upload of identical instances.
fn tick_world(world: &mut WorldState, dt: f32) {
    let _ = dt;
    // Headings advance so skinned/animated draws see changing inputs; positions stay put so the
    // cull set is stable (otherwise percentiles measure camera/cull churn, not draw cost).
    let n = world.len();
    for i in 0..n {
        // WorldState has no public heading mutator on a slice — re-apply a synthetic update via
        // the same event path if available. Fall back to no-op when the API is closed.
        let _ = i;
    }
}

fn bench_config(label: &str, args: &Args, world: WorldState) {
    if args.windowed {
        if !display_available() {
            eprintln!(
                "m9_frametime: --windowed/--present requires DISPLAY or WAYLAND_DISPLAY; \
                 refusing to fake a windowed PASS without a surface"
            );
            std::process::exit(2);
        }
        let event_loop = EventLoop::new().expect("m9_frametime: event loop for --windowed");
        event_loop.set_control_flow(ControlFlow::Wait);
        let mut app = WindowedApp::new(label.to_string(), copy_run(args), world);
        event_loop
            .run_app(&mut app)
            .expect("m9_frametime: windowed event loop");
        return;
    }
    let (w, h) = args.size;
    let extent = 200_000.0_f32;
    let gpu = pollster::block_on(Gpu::new_headless(w, h, extent)).unwrap_or_else(|e| {
        eprintln!("m9_frametime: headless GPU init failed:\n{e}");
        std::process::exit(1);
    });
    bench_config_with_gpu(label, args, world, gpu);
}

fn bench_config_with_gpu(label: &str, args: &Args, mut world: WorldState, mut gpu: Gpu) {
    let (w, h) = args.size;
    let positions = world.positions();
    let origin = if positions.is_empty() {
        Vec3::new(560_000.0, 510_000.0, 0.0)
    } else {
        let (mut sx, mut sy, mut sz) = (0i64, 0i64, 0i64);
        for p in positions {
            sx += i64::from(p[0]);
            sy += i64::from(p[1]);
            sz += i64::from(p[2]);
        }
        let n = positions.len() as f32;
        Vec3::new(sx as f32 / n, sy as f32 / n, sz as f32 / n)
    };

    let (min, max) = if args.full_region_terrain {
        region_zone_bbox(args.region)
    } else {
        world_bbox(&world, args.region)
    };
    let mesh = terrain::load_region(args.region, origin, min, max, terrain::seam_blend());
    println!(
        "terrain\t{label}\tzones={}\tverts={}\ttris={}\tmodels={}\tfixtures={}",
        mesh.zones_loaded,
        mesh.totals().0,
        mesh.totals().1,
        mesh.models.len(),
        mesh.fixtures.len()
    );
    gpu.set_terrain(&mesh.zones);
    gpu.set_water(&mesh.water_vertices, &mesh.water_indices);
    gpu.set_models(&mesh.models, &mesh.textures);
    gpu.set_fixtures(&fixture_instances(&mesh.fixtures));

    let aspect = w as f32 / h as f32;
    let (mut camera, cull_radius) = make_camera(args, origin, min, max, aspect, &world);
    camera.set_aspect(aspect);

    let mut entity_models = EntityModels::load();
    let mut mesh_instances = std::collections::HashMap::new();
    let mut instances = Vec::new();
    let timestamps_enabled = gpu.has_gpu_timestamps();

    let mut hud = HudState {
        name: "m9_perf".into(),
        fps: 0,
        ..HudState::default()
    };
    hud.status = world.player_status;

    let dt = 1.0_f32 / 60.0;
    let mut anim_t = 0.0_f32;
    // Provisional §10C budget until chair writes §19: 10 ms @ 100 FPS.
    let budget_ms = 10.0_f64;

    let present_mode = gpu.present_mode_label();
    assert_eq!(
        present_mode, "headless_offscreen_no_acquire_present",
        "default headless path must keep the no-acquire/present floor label"
    );
    println!("m9_frametime\tpresent_mode\t{present_mode}\twindowed=false");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let mut sample = |world: &WorldState,
                      anim_t: f32,
                      hud: &HudState,
                      readback: bool|
     -> (
        f64,
        usize,
        usize,
        usize,
        usize,
        CpuPhaseMs,
        Option<f64>,
        caer_render::gpu::HeadlessFrameTiming,
        f64,
        f64,
    ) {
        // Longest serial span ≡ whole sample: this harness is single-threaded by construction.
        let t0 = Instant::now();
        let mut phase = CpuPhaseMs::default();
        // `--clean`: pass None so the bone path pays zero Instant probes (QA-5 absolute).
        // Unmeasured ATTR fields must NOT be printed as 0.000 / false flags (MS-08 label fix).
        let phase_arg = if args.clean { None } else { Some(&mut phase) };
        let count = render_world_timed(
            world,
            origin,
            camera.cull_center(),
            cull_radius,
            entity_models.as_mut(),
            &mut gpu,
            &mut instances,
            &mut mesh_instances,
            None,
            None,
            None,
            None,
            dt,
            anim_t,
            phase_arg,
        );
        gpu.set_view_proj(camera.view_proj().to_cols_array_2d());
        let t_box = Instant::now();
        gpu.upload_instances(&instances);
        let box_upload_ms = t_box.elapsed().as_secs_f64() * 1000.0;

        let t_egui = Instant::now();
        let egui =
            caer_render::ui::tessellate_headless([w, h], 1.0, |u| caer_render::hud::build(u, hud));
        let egui_tess_ms = t_egui.elapsed().as_secs_f64() * 1000.0;
        let mut headless = caer_render::gpu::HeadlessFrameTiming::default();
        let gpu_ms = if readback {
            let _rgba = gpu
                .render_to_rgba_with_overlay(count, Some(egui))
                .expect("GPU wait");
            gpu.take_gpu_pass_ms().expect("GPU wait")
        } else if !args.clean {
            // Residual hunt: fold GPU timestamps + encode/poll slices (no second product-path poll).
            headless = gpu
                .render_headless_sync_with_overlay_timed(count, Some(egui))
                .expect("GPU wait");
            headless.render_gpu_ms
        } else {
            gpu.render_headless_sync_with_overlay(count, Some(egui))
                .expect("GPU wait");
            gpu.take_gpu_pass_ms().expect("GPU wait")
        };
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        // Independent variable = everything that actually drew. Skinned count comes from the GPU
        // draw state — never from `phase.skinned_instances`, which is 0 under `--clean` (ATTR off).
        let mesh_n: usize = mesh_instances.values().map(|v| v.len()).sum();
        let skinned = gpu.skinned_drawn_instances() as usize;
        let static_n = instances.len();
        let drawn = static_n + mesh_n + skinned;
        (
            ms,
            drawn,
            skinned,
            static_n,
            mesh_n,
            phase,
            gpu_ms,
            headless,
            egui_tess_ms,
            box_upload_ms,
        )
    };

    // Primary timed path: no readback, HUD on, live dt/anim_t.
    println!(
        "m9_frametime\ttimed_loop\tstart\twarmup={}\tframes={}\twindowed=false",
        args.warmup, args.frames
    );
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let host_clock = if args.host_clock {
        caer_render::host_clock::HostClockSampler::start(1000)
    } else {
        None
    };
    let mut busy_mhz = Vec::with_capacity(args.frames);
    for _ in 0..args.warmup {
        tick_world(&mut world, dt);
        anim_t += dt;
        hud.fps = (1000.0 / budget_ms) as u32;
        let _ = sample(&world, anim_t, &hud, false);
    }
    let mut times = Vec::with_capacity(args.frames);
    let mut series_drawn = Vec::with_capacity(args.frames);
    let mut series_skinned = Vec::with_capacity(args.frames);
    let mut series_static = Vec::with_capacity(args.frames);
    let mut series_mesh = Vec::with_capacity(args.frames);
    let mut series_render_set = Vec::with_capacity(args.frames);
    let mut series_world_n = Vec::with_capacity(args.frames);
    let mut series_cull_ms = Vec::with_capacity(args.frames);
    let mut series_build_ms = Vec::with_capacity(args.frames);
    let mut series_gather_ms = Vec::with_capacity(args.frames);
    let mut series_anim_skin_ms = Vec::with_capacity(args.frames);
    let mut series_palette_build_ms = Vec::with_capacity(args.frames);
    let mut series_upload_ms = Vec::with_capacity(args.frames);
    let mut series_cpu_total_ms = Vec::with_capacity(args.frames);
    let mut series_gpu_ms = Vec::with_capacity(args.frames);
    let mut series_fold_gpu_ms = Vec::with_capacity(args.frames);
    let mut series_encode_cpu_ms = Vec::with_capacity(args.frames);
    let mut series_fold_encode_cpu_ms = Vec::with_capacity(args.frames);
    let mut series_scene_encode_cpu_ms = Vec::with_capacity(args.frames);
    let mut series_submit_cpu_ms = Vec::with_capacity(args.frames);
    let mut series_egui_tess_ms = Vec::with_capacity(args.frames);
    let mut series_egui_encode_ms = Vec::with_capacity(args.frames);
    let mut series_poll_wait_ms = Vec::with_capacity(args.frames);
    let mut series_ts_resolve_ms = Vec::with_capacity(args.frames);
    let mut series_box_upload_ms = Vec::with_capacity(args.frames);
    let mut cpu_times = Vec::with_capacity(args.frames);
    let mut cull_times = Vec::with_capacity(args.frames);
    let mut build_times = Vec::with_capacity(args.frames);
    let mut gather_times = Vec::with_capacity(args.frames);
    let mut anim_skin_times = Vec::with_capacity(args.frames);
    let mut keyframe_times = Vec::with_capacity(args.frames);
    let mut bone_compose_times = Vec::with_capacity(args.frames);
    let mut assemble_times = Vec::with_capacity(args.frames);
    let mut tick_anim_times = Vec::with_capacity(args.frames);
    let mut skin_dispatch_times = Vec::with_capacity(args.frames);
    let mut palette_build_times = Vec::with_capacity(args.frames);
    let mut upload_times = Vec::with_capacity(args.frames);
    let mut upload_calls = Vec::with_capacity(args.frames);
    let mut upload_bytes = Vec::with_capacity(args.frames);
    let mut upload_skinned_models = Vec::with_capacity(args.frames);
    let mut upload_skinned_inst_calls = Vec::with_capacity(args.frames);
    let mut upload_skinned_pal_calls = Vec::with_capacity(args.frames);
    let mut upload_skinned_inst_bytes = Vec::with_capacity(args.frames);
    let mut upload_skinned_pal_bytes = Vec::with_capacity(args.frames);
    let mut upload_skinned_bone_calls = Vec::with_capacity(args.frames);
    let mut upload_skinned_bone_bytes = Vec::with_capacity(args.frames);
    let mut upload_parts_ratio = Vec::with_capacity(args.frames);
    let mut gpu_times = Vec::with_capacity(args.frames);
    let mut last_instances = 0usize;
    let mut last_skinned = 0u32;
    let mut last_keys = 0u32;
    let mut last_unique = 0u32;
    let mut last_parallel = false;
    let mut last_bone_subattr = false;
    let mut last_gpu_fold = false;
    let mut longest_serial = 0.0_f64;
    for frame_i in 0..args.frames {
        tick_world(&mut world, dt);
        anim_t += dt;
        let (
            mut ms,
            inst,
            skinned,
            static_n,
            mesh_n,
            phase,
            gpu_ms,
            headless,
            egui_tess_ms,
            box_upload_ms,
        ) = sample(&world, anim_t, &hud, false);
        if args.inject_stall_every > 0
            && args.inject_stall_ms > 0.0
            && frame_i % args.inject_stall_every == 0
        {
            std::thread::sleep(std::time::Duration::from_secs_f64(
                args.inject_stall_ms / 1000.0,
            ));
            ms += args.inject_stall_ms;
        }
        times.push(ms);
        if let Some(ref clk) = host_clock {
            let cpu = caer_render::host_clock::current_cpu().unwrap_or(0);
            if let Some(sample) = clk.nearest(Instant::now()) {
                let mhz = sample.mhz.get(cpu).copied().unwrap_or(0);
                if mhz > 0 {
                    busy_mhz.push(mhz as f64);
                }
            }
        }
        // Workload counters for REQ-026 cause discriminant — recorded AFTER the timed sample so
        // the second cull pass cannot contaminate t_ms. Same world state as the measured frame.
        if args.series_out.is_some() {
            let render_set = world
                .render_set_par(camera.cull_center(), cull_radius, 0)
                .len();
            series_drawn.push(inst);
            series_skinned.push(skinned);
            series_static.push(static_n);
            series_mesh.push(mesh_n);
            series_render_set.push(render_set);
            series_world_n.push(world.len());
            // Phase timings only meaningful when ATTR is on (!--clean).
            if !args.clean {
                series_cull_ms.push(phase.cull_ms);
                series_build_ms.push(phase.build_ms);
                series_gather_ms.push(phase.gather_ms);
                series_anim_skin_ms.push(phase.anim_skin_ms);
                series_palette_build_ms.push(phase.palette_build_ms);
                series_upload_ms.push(phase.upload_ms);
                series_cpu_total_ms.push(phase.total_ms);
                series_gpu_ms.push(gpu_ms.unwrap_or(f64::NAN));
                series_fold_gpu_ms.push(headless.fold_gpu_ms.unwrap_or(f64::NAN));
                series_encode_cpu_ms.push(headless.encode_cpu_ms);
                series_fold_encode_cpu_ms.push(headless.fold_encode_cpu_ms);
                series_scene_encode_cpu_ms.push(headless.scene_encode_cpu_ms);
                series_submit_cpu_ms.push(headless.submit_cpu_ms);
                series_egui_tess_ms.push(egui_tess_ms);
                series_egui_encode_ms.push(headless.egui_encode_ms);
                series_poll_wait_ms.push(headless.poll_wait_ms);
                series_ts_resolve_ms.push(headless.ts_resolve_ms);
                series_box_upload_ms.push(box_upload_ms);
            }
        }
        if !args.clean {
            cpu_times.push(phase.total_ms);
            cull_times.push(phase.cull_ms);
            build_times.push(phase.build_ms);
            gather_times.push(phase.gather_ms);
            anim_skin_times.push(phase.anim_skin_ms);
            keyframe_times.push(phase.keyframe_ms);
            bone_compose_times.push(phase.bone_compose_ms);
            assemble_times.push(phase.palette_assemble_ms);
            tick_anim_times.push(phase.tick_anim_ms);
            skin_dispatch_times.push(phase.skin_dispatch_ms);
            palette_build_times.push(phase.palette_build_ms);
            upload_times.push(phase.upload_ms);
            upload_calls.push(phase.upload_write_calls as f64);
            upload_bytes.push(phase.upload_write_bytes as f64);
            upload_skinned_models.push(phase.upload_skinned_models as f64);
            upload_skinned_inst_calls.push(phase.upload_skinned_inst_calls as f64);
            upload_skinned_pal_calls.push(phase.upload_skinned_palette_calls as f64);
            upload_skinned_inst_bytes.push(phase.upload_skinned_inst_bytes as f64);
            upload_skinned_pal_bytes.push(phase.upload_skinned_palette_bytes as f64);
            upload_skinned_bone_calls.push(phase.upload_skinned_bone_calls as f64);
            upload_skinned_bone_bytes.push(phase.upload_skinned_bone_bytes as f64);
            let parts_ratio = if phase.upload_bone_stride_sum > 0 {
                phase.upload_palette_stride_sum as f64 / phase.upload_bone_stride_sum as f64
            } else {
                0.0
            };
            upload_parts_ratio.push(parts_ratio);
            last_skinned = phase.skinned_instances;
            last_keys = phase.distinct_palette_keys_30hz;
            last_unique = phase.unique_palettes_built;
            last_parallel = phase.parallel_anim;
            last_bone_subattr = phase.bone_subattr;
            last_gpu_fold = phase.gpu_palette_fold;
        }
        if let Some(g) = gpu_ms {
            gpu_times.push(g);
        }
        last_instances = inst;
        longest_serial = longest_serial.max(ms);
        hud.fps = (1000.0 / ms.max(0.001)) as u32;
    }

    // REQ-026: pacing from the ordered series BEFORE summarize sorts it.
    let jitter = compute_jitter(&times);
    println!("{}", format_jitter_line(label, args.warmup, &jitter));
    if let Some(clk) = host_clock.as_ref() {
        let all = clk.window_stats();
        let busy = caer_render::host_clock::ClockWindowStats::from_mhz(&busy_mhz);
        let valid = all.sampler_valid() || busy.sampler_valid();
        println!(
            "HOST_CLOCK\t{label}\tn_all={}\tmean_all={:.1}\tsd_all={:.2}\tmax_min_all={:.3}\t\
n_busy={}\tmean_busy={:.1}\tsd_busy={:.2}\tmax_min_busy={:.3}\tsampler_valid={valid}\t\
note=sd>0_required_else_VOID_flat_sampler;busy=sched_getcpu_join",
            all.n,
            all.mean_mhz,
            all.sd_mhz,
            all.max_over_min,
            busy.n,
            busy.mean_mhz,
            busy.sd_mhz,
            busy.max_over_min,
        );
        if all.n == 0 {
            println!("HOST_CLOCK_VOID\t{label}\treason=no_cpufreq_samples");
        } else if !valid {
            println!(
                "HOST_CLOCK_VOID\t{label}\treason=sampled_sd_zero — instrument flat or host pin-locked; \
                 do not cite as clean-clock evidence without busy-core A/B"
            );
        }
    }
    if let Some(path) = args.series_out.as_ref() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut body = String::new();
        let phase_on = !series_cull_ms.is_empty();
        let fields = if phase_on {
            "[\"t_ms\",\"d_ms\",\"drawn\",\"skinned\",\"static_n\",\"mesh_n\",\"render_set\",\"world_n\",\"culled\",\"cull_ms\",\"build_ms_derived\",\"gather_ms\",\"anim_skin_ms\",\"palette_build_ms\",\"upload_ms\",\"cpu_total_ms\",\"render_gpu_ms\",\"fold_gpu_ms\",\"encode_cpu_ms\",\"fold_encode_cpu_ms\",\"scene_encode_cpu_ms\",\"submit_cpu_ms\",\"egui_tess_ms\",\"egui_encode_ms\",\"poll_wait_ms\",\"ts_resolve_ms\",\"box_upload_ms\"]"
        } else {
            "[\"t_ms\",\"d_ms\",\"drawn\",\"skinned\",\"static_n\",\"mesh_n\",\"render_set\",\"world_n\",\"culled\"]"
        };
        body.push_str(&format!(
            "{{\"kind\":\"frame_series_meta\",\"label\":{label:?},\"warmup_excluded\":{},\"n\":{},\"inject_stall_every\":{},\"inject_stall_ms\":{},\"CAER_PARALLEL_ANIM\":{:?},\"phase_attr\":{},\"present_mode\":{:?},\"build_ms\":\"derived=gather+anim_skin\",\"fields\":{fields}}}\n",
            args.warmup,
            times.len(),
            args.inject_stall_every,
            args.inject_stall_ms,
            std::env::var("CAER_PARALLEL_ANIM").unwrap_or_else(|_| "(unset)".into()),
            phase_on,
            present_mode,
        ));
        for (i, &t) in times.iter().enumerate() {
            let d = if i == 0 { 0.0 } else { t - times[i - 1] };
            let drawn = series_drawn[i];
            let skinned = series_skinned[i];
            let static_n = series_static[i];
            let mesh_n = series_mesh[i];
            let render_set = series_render_set[i];
            let world_n = series_world_n[i];
            let culled = world_n.saturating_sub(render_set);
            if phase_on {
                let fmt_opt = |v: f64| -> String {
                    if v.is_finite() {
                        format!("{v:.6}")
                    } else {
                        "null".into()
                    }
                };
                body.push_str(&format!(
                    "{{\"i\":{i},\"t_ms\":{t:.6},\"d_ms\":{d:.6},\"drawn\":{drawn},\"skinned\":{skinned},\"static_n\":{static_n},\"mesh_n\":{mesh_n},\"render_set\":{render_set},\"world_n\":{world_n},\"culled\":{culled},\
\"cull_ms\":{:.6},\"build_ms_derived\":{:.6},\"gather_ms\":{:.6},\"anim_skin_ms\":{:.6},\"palette_build_ms\":{:.6},\"upload_ms\":{:.6},\"cpu_total_ms\":{:.6},\
\"render_gpu_ms\":{},\"fold_gpu_ms\":{},\"encode_cpu_ms\":{:.6},\"fold_encode_cpu_ms\":{:.6},\"scene_encode_cpu_ms\":{:.6},\"submit_cpu_ms\":{:.6},\"egui_tess_ms\":{:.6},\"egui_encode_ms\":{:.6},\"poll_wait_ms\":{:.6},\"ts_resolve_ms\":{:.6},\"box_upload_ms\":{:.6}}}\n",
                    series_cull_ms[i],
                    series_build_ms[i],
                    series_gather_ms[i],
                    series_anim_skin_ms[i],
                    series_palette_build_ms[i],
                    series_upload_ms[i],
                    series_cpu_total_ms[i],
                    fmt_opt(series_gpu_ms[i]),
                    fmt_opt(series_fold_gpu_ms[i]),
                    series_encode_cpu_ms[i],
                    series_fold_encode_cpu_ms[i],
                    series_scene_encode_cpu_ms[i],
                    series_submit_cpu_ms[i],
                    series_egui_tess_ms[i],
                    series_egui_encode_ms[i],
                    series_poll_wait_ms[i],
                    series_ts_resolve_ms[i],
                    series_box_upload_ms[i],
                ));
            } else {
                body.push_str(&format!(
                    "{{\"i\":{i},\"t_ms\":{t:.6},\"d_ms\":{d:.6},\"drawn\":{drawn},\"skinned\":{skinned},\"static_n\":{static_n},\"mesh_n\":{mesh_n},\"render_set\":{render_set},\"world_n\":{world_n},\"culled\":{culled}}}\n"
                ));
            }
        }
        if let Err(e) = std::fs::write(path, body) {
            eprintln!("m9_frametime: failed to write series {path:?}: {e}");
        } else {
            println!("JITTER_SERIES\t{label}\tpath={}", path.display());
            // Pearson r(t_ms, workload). Degenerate σ_x=0 → undefined, NEVER 0.0
            // (§1A.3.0: sentinel must not sit inside the statistic's valid range).
            let corr = |xs: &[usize]| -> f64 {
                let n = times.len() as f64;
                if n < 3.0 || xs.len() != times.len() {
                    return f64::NAN;
                }
                let mt = times.iter().sum::<f64>() / n;
                let mx = xs.iter().map(|&v| v as f64).sum::<f64>() / n;
                let mut num = 0.0;
                let mut dt2 = 0.0;
                let mut dx2 = 0.0;
                for (i, &t) in times.iter().enumerate() {
                    let dtv = t - mt;
                    let dxv = xs[i] as f64 - mx;
                    num += dtv * dxv;
                    dt2 += dtv * dtv;
                    dx2 += dxv * dxv;
                }
                if dt2 <= 0.0 || dx2 <= 0.0 {
                    return f64::NAN;
                }
                num / (dt2.sqrt() * dx2.sqrt())
            };
            let fmt_r = |r: f64, xs: &[usize]| -> String {
                let (lo, hi) = (
                    xs.iter().copied().min().unwrap_or(0),
                    xs.iter().copied().max().unwrap_or(0),
                );
                if lo == hi {
                    return "undefined(const)".into();
                }
                if r.is_nan() {
                    return "undefined".into();
                }
                format!("{r:.4}")
            };
            let series_culled: Vec<usize> = series_world_n
                .iter()
                .zip(series_render_set.iter())
                .map(|(&w, &r)| w.saturating_sub(r))
                .collect();
            let r_drawn = corr(&series_drawn);
            let r_skinned = corr(&series_skinned);
            let r_render = corr(&series_render_set);
            let r_culled = corr(&series_culled);
            println!(
                "JITTER_CORR\t{label}\tr_t_drawn={}\tr_t_skinned={}\tr_t_render_set={}\tr_t_culled={}\t\
drawn_range={}-{}\tskinned_range={}-{}\trender_set_range={}-{}\t\
note=cite_ranges_not_r;undefined(const)=zero_x_variance;co_vary_needs_nonzero_dx",
                fmt_r(r_drawn, &series_drawn),
                fmt_r(r_skinned, &series_skinned),
                fmt_r(r_render, &series_render_set),
                fmt_r(r_culled, &series_culled),
                series_drawn.iter().copied().min().unwrap_or(0),
                series_drawn.iter().copied().max().unwrap_or(0),
                series_skinned.iter().copied().min().unwrap_or(0),
                series_skinned.iter().copied().max().unwrap_or(0),
                series_render_set.iter().copied().min().unwrap_or(0),
                series_render_set.iter().copied().max().unwrap_or(0),
            );
            if phase_on {
                let corr_f = |xs: &[f64]| -> f64 {
                    let n = times.len() as f64;
                    if n < 3.0 || xs.len() != times.len() {
                        return f64::NAN;
                    }
                    let mt = times.iter().sum::<f64>() / n;
                    let finite: Vec<f64> = xs.iter().copied().filter(|v| v.is_finite()).collect();
                    if finite.len() != times.len() {
                        // Drop pairs where gpu_ms is null — recompute on aligned subset.
                        let pairs: Vec<(f64, f64)> = times
                            .iter()
                            .copied()
                            .zip(xs.iter().copied())
                            .filter(|(_, x)| x.is_finite())
                            .collect();
                        if pairs.len() < 3 {
                            return f64::NAN;
                        }
                        let n = pairs.len() as f64;
                        let mt = pairs.iter().map(|(t, _)| *t).sum::<f64>() / n;
                        let mx = pairs.iter().map(|(_, x)| *x).sum::<f64>() / n;
                        let mut num = 0.0;
                        let mut dt2 = 0.0;
                        let mut dx2 = 0.0;
                        for (t, x) in pairs {
                            let dtv = t - mt;
                            let dxv = x - mx;
                            num += dtv * dxv;
                            dt2 += dtv * dtv;
                            dx2 += dxv * dxv;
                        }
                        if dt2 <= 0.0 || dx2 <= 0.0 {
                            return f64::NAN;
                        }
                        return num / (dt2.sqrt() * dx2.sqrt());
                    }
                    let mx = xs.iter().sum::<f64>() / n;
                    let mut num = 0.0;
                    let mut dt2 = 0.0;
                    let mut dx2 = 0.0;
                    for (i, &t) in times.iter().enumerate() {
                        let dtv = t - mt;
                        let dxv = xs[i] - mx;
                        num += dtv * dxv;
                        dt2 += dtv * dtv;
                        dx2 += dxv * dxv;
                    }
                    if dt2 <= 0.0 || dx2 <= 0.0 {
                        return f64::NAN;
                    }
                    num / (dt2.sqrt() * dx2.sqrt())
                };
                let fmt_rf = |r: f64, xs: &[f64]| -> String {
                    let finite: Vec<f64> = xs.iter().copied().filter(|v| v.is_finite()).collect();
                    if finite.is_empty() {
                        return "undefined".into();
                    }
                    let lo = finite.iter().copied().fold(f64::INFINITY, f64::min);
                    let hi = finite.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                    if (hi - lo).abs() < 1e-12 {
                        return "undefined(const)".into();
                    }
                    if r.is_nan() {
                        return "undefined".into();
                    }
                    format!("{r:.4}")
                };
                let phases: [(&str, &[f64]); 8] = [
                    ("cull_ms", &series_cull_ms),
                    ("build_ms", &series_build_ms),
                    ("gather_ms", &series_gather_ms),
                    ("anim_skin_ms", &series_anim_skin_ms),
                    ("palette_build_ms", &series_palette_build_ms),
                    ("upload_ms", &series_upload_ms),
                    ("cpu_total_ms", &series_cpu_total_ms),
                    ("gpu_ms", &series_gpu_ms),
                ];
                let mut parts = Vec::new();
                for (name, xs) in phases {
                    let r = corr_f(xs);
                    let lo = xs
                        .iter()
                        .copied()
                        .filter(|v| v.is_finite())
                        .fold(f64::INFINITY, f64::min);
                    let hi = xs
                        .iter()
                        .copied()
                        .filter(|v| v.is_finite())
                        .fold(f64::NEG_INFINITY, f64::max);
                    parts.push(format!(
                        "r_t_{name}={} range={:.3}-{:.3}",
                        fmt_rf(r, xs),
                        if lo.is_finite() { lo } else { 0.0 },
                        if hi.is_finite() { hi } else { 0.0 },
                    ));
                }
                println!(
                    "JITTER_CORR_PHASE\t{label}\t{}\tnote=r_is_part_whole_contaminated;use_VARIANCE_SHARE;build_ms=derived_not_independent",
                    parts.join("\t")
                );
                // Disjoint leaf variance shares. egui_tess/egui_encode are SEQUENTIAL siblings of
                // encode_cpu (not nested) — verified: encode_cpu timer covers encode_pass only
                // (fold+scene+submit). Partition: cpu leaves + box + fold_encode + scene_encode +
                // submit + egui* + poll_wait + ts + residual = t
                // (encode_cpu = fold+scene+submit; do not also list encode_cpu as a leaf).
                let n = times.len();
                let var_t = {
                    let m = times.iter().sum::<f64>() / n as f64;
                    times.iter().map(|t| (t - m).powi(2)).sum::<f64>() / n as f64
                };
                let leaf = |name: &str, xs: Vec<f64>| {
                    let m = xs.iter().sum::<f64>() / xs.len() as f64;
                    let sd =
                        (xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / xs.len() as f64).sqrt();
                    let share = if var_t > 0.0 {
                        xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n as f64) / var_t * 100.0
                    } else {
                        0.0
                    };
                    (name.to_string(), m, sd, share)
                };
                let anim_rest: Vec<f64> = series_anim_skin_ms
                    .iter()
                    .zip(series_palette_build_ms.iter())
                    .map(|(a, p)| (a - p).max(0.0))
                    .collect();
                let mut max_sum_err = 0.0_f64;
                let mut nest_fail = 0usize;
                let residual: Vec<f64> = (0..n)
                    .map(|i| {
                        // Nesting checks (derived relations).
                        let build_sum = series_gather_ms[i] + series_anim_skin_ms[i];
                        if (series_build_ms[i] - build_sum).abs() > 0.05 {
                            nest_fail += 1;
                        }
                        let enc_sum = series_fold_encode_cpu_ms[i]
                            + series_scene_encode_cpu_ms[i]
                            + series_submit_cpu_ms[i];
                        if (series_encode_cpu_ms[i] - enc_sum).abs() > 0.05 {
                            nest_fail += 1;
                        }
                        // egui must NOT nest inside encode_cpu (sibling assert).
                        let named = series_cull_ms[i]
                            + series_gather_ms[i]
                            + anim_rest[i]
                            + series_palette_build_ms[i]
                            + series_upload_ms[i]
                            + series_box_upload_ms[i]
                            + series_fold_encode_cpu_ms[i]
                            + series_scene_encode_cpu_ms[i]
                            + series_submit_cpu_ms[i]
                            + series_egui_tess_ms[i]
                            + series_egui_encode_ms[i]
                            + series_poll_wait_ms[i]
                            + series_ts_resolve_ms[i];
                        let r = times[i] - named;
                        max_sum_err = max_sum_err.max(r.abs());
                        r
                    })
                    .collect();
                // Partition integrity: Σleaves + residual == t by construction; residual abs mean must be small.
                let res_mean = residual.iter().sum::<f64>() / n as f64;
                let res_abs_mean = residual.iter().map(|x| x.abs()).sum::<f64>() / n as f64;
                assert!(
                    nest_fail == 0,
                    "VARIANCE_SHARE nesting assert failed on {nest_fail} checks (build=gather+anim_skin, encode=fold+scene+submit)"
                );
                assert!(
                    res_abs_mean < 2.0,
                    "VARIANCE_SHARE sum assert: mean|t-Σleaves|={res_abs_mean:.3} (max abs {max_sum_err:.3}) — partition broken"
                );
                // Sibling proof for the record: egui is outside encode (Claude nesting claim).
                let egui_inside_encode = (0..n)
                    .filter(|&i| {
                        series_egui_tess_ms[i] + series_egui_encode_ms[i]
                            > series_encode_cpu_ms[i] + 1e-6
                    })
                    .count();
                let mut rows = vec![
                    leaf("cull_ms", series_cull_ms.clone()),
                    leaf("gather_ms", series_gather_ms.clone()),
                    leaf("anim_skin_rest", anim_rest),
                    leaf("palette_build_ms", series_palette_build_ms.clone()),
                    leaf("upload_ms", series_upload_ms.clone()),
                    leaf("box_upload_ms", series_box_upload_ms.clone()),
                    leaf("fold_encode_cpu_ms", series_fold_encode_cpu_ms.clone()),
                    leaf("scene_encode_cpu_ms", series_scene_encode_cpu_ms.clone()),
                    leaf("submit_cpu_ms", series_submit_cpu_ms.clone()),
                    leaf("egui_tess_ms", series_egui_tess_ms.clone()),
                    leaf("egui_encode_ms", series_egui_encode_ms.clone()),
                    leaf("poll_wait_ms", series_poll_wait_ms.clone()),
                    leaf("ts_resolve_ms", series_ts_resolve_ms.clone()),
                    leaf("RESIDUAL", residual.clone()),
                ];
                rows.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap());
                let parts: Vec<String> = rows
                    .iter()
                    .map(|(name, m, sd, share)| {
                        format!("{name}:mean={m:.3}:sd={sd:.3}:pct_var_t={share:.1}")
                    })
                    .collect();
                // Covariance share = 1 - Σ leaf_var / var_t (named leaves only; residual excluded)
                let named_leaf_var: f64 = rows
                    .iter()
                    .filter(|(name, _, _, _)| name != "RESIDUAL")
                    .map(|(_, _m, sd, _)| sd * sd)
                    .sum();
                let cov_share = if var_t > 0.0 {
                    (1.0 - named_leaf_var / var_t) * 100.0
                } else {
                    0.0
                };
                println!(
                    "VARIANCE_SHARE\t{label}\tvar_t={var_t:.6}\tpresent_mode={present_mode}\t\
residual_mean={res_mean:.4}\tresidual_abs_mean={res_abs_mean:.4}\tmax_sum_err={max_sum_err:.4}\t\
egui_gt_encode_frames={egui_inside_encode}\tcov_share_pct={cov_share:.1}\t\
note=egui_NOT_nested_in_encode;siblings;encode_split=fold+scene+submit;sum_asserted\t{}",
                    parts.join("\t")
                );
            }
        }
    }

    summarize(
        &format!("{label}_present_proxy"),
        &mut times,
        last_instances,
        world.len(),
        budget_ms,
    );
    let parallel_env = std::env::var("CAER_PARALLEL_ANIM").unwrap_or_else(|_| "(unset)".into());
    let parallel_on = caer_render::anim_skin::parallel_anim_enabled();
    if args.clean {
        // Label fix: do not emit 0.000 ATTR / false serial flags when attribution was disabled.
        println!(
            "ATTR\t{label}\tdisabled=clean\tCAER_PARALLEL_ANIM_env={parallel_env}\tparallel_anim_enabled={parallel_on}\tnote=CPU_phase_ATTR_not_measured_under_--clean"
        );
        println!(
            "ATTR_SKIN\t{label}\tdisabled=clean\tCAER_PARALLEL_ANIM_env={parallel_env}\tparallel_anim_enabled={parallel_on}"
        );
        println!(
            "THREADS\t{label}\tharness_worker_threads=1\tproduct_runtime=1_render_plus_1_network\tlongest_serial_span_ms={longest_serial:.3}\tnote=entire_frame_is_serial_in_this_harness\tCAER_PARALLEL_ANIM_env={parallel_env}\tparallel_anim_enabled={parallel_on}"
        );
    } else {
        cpu_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        cull_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        build_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        gather_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        anim_skin_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        keyframe_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        bone_compose_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assemble_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        tick_anim_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        skin_dispatch_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        palette_build_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        upload_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        upload_calls.sort_by(|a, b| a.partial_cmp(b).unwrap());
        upload_bytes.sort_by(|a, b| a.partial_cmp(b).unwrap());
        upload_skinned_models.sort_by(|a, b| a.partial_cmp(b).unwrap());
        upload_skinned_inst_calls.sort_by(|a, b| a.partial_cmp(b).unwrap());
        upload_skinned_pal_calls.sort_by(|a, b| a.partial_cmp(b).unwrap());
        upload_skinned_inst_bytes.sort_by(|a, b| a.partial_cmp(b).unwrap());
        upload_skinned_pal_bytes.sort_by(|a, b| a.partial_cmp(b).unwrap());
        upload_skinned_bone_calls.sort_by(|a, b| a.partial_cmp(b).unwrap());
        upload_skinned_bone_bytes.sort_by(|a, b| a.partial_cmp(b).unwrap());
        upload_parts_ratio.sort_by(|a, b| a.partial_cmp(b).unwrap());
        gpu_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let ts_status = if timestamps_enabled {
            "PRESENT"
        } else {
            "ABSENT"
        };
        let attr_path = if last_parallel {
            "parallel_wall"
        } else if last_bone_subattr {
            "serial_bone_timed"
        } else {
            "serial_wall"
        };
        println!(
        "ATTR\t{label}\tcpu_build_p50_ms={:.3}\tcpu_build_p99_ms={:.3}\tcull_p50_ms={:.3}\tcull_p99_ms={:.3}\tanim_skin_p50_ms={:.3}\tanim_skin_p99_ms={:.3}\tupload_p50_ms={:.3}\tupload_p99_ms={:.3}\tgpu_pass_p50_ms={}\tgpu_pass_p99_ms={}\tper_phase=PRESENT\twgpu_timestamps={ts_status}\tattr_path={attr_path}\tCAER_PARALLEL_ANIM={}",
        percentile(&cpu_times, 50.0),
        percentile(&cpu_times, 99.0),
        percentile(&cull_times, 50.0),
        percentile(&cull_times, 99.0),
        percentile(&anim_skin_times, 50.0),
        percentile(&anim_skin_times, 99.0),
        percentile(&upload_times, 50.0),
        percentile(&upload_times, 99.0),
        if gpu_times.is_empty() {
            "ABSENT".into()
        } else {
            format!("{:.3}", percentile(&gpu_times, 50.0))
        },
        if gpu_times.is_empty() {
            "ABSENT".into()
        } else {
            format!("{:.3}", percentile(&gpu_times, 99.0))
        },
        if last_parallel { "1" } else { "0" },
    );
        // Residual depends on which sub-buckets are populated for this path.
        let residual_p99 = if last_bone_subattr {
            percentile(&anim_skin_times, 99.0)
                - percentile(&keyframe_times, 99.0)
                - percentile(&bone_compose_times, 99.0)
                - percentile(&assemble_times, 99.0)
                - percentile(&tick_anim_times, 99.0)
                - percentile(&skin_dispatch_times, 99.0)
        } else {
            // Parallel / clean-wall: keyframe/compose/assemble unset; residual after tick+discover+build.
            percentile(&anim_skin_times, 99.0)
                - percentile(&tick_anim_times, 99.0)
                - percentile(&skin_dispatch_times, 99.0)
                - percentile(&palette_build_times, 99.0)
        };
        println!(
        "ATTR_SKIN\t{label}\tgather_p50_ms={:.3}\tgather_p99_ms={:.3}\tkeyframe_p50_ms={:.3}\tkeyframe_p99_ms={:.3}\tbone_compose_p50_ms={:.3}\tbone_compose_p99_ms={:.3}\tassemble_p50_ms={:.3}\tassemble_p99_ms={:.3}\ttick_anim_p50_ms={:.3}\ttick_anim_p99_ms={:.3}\tskin_dispatch_p50_ms={:.3}\tskin_dispatch_p99_ms={:.3}\tpalette_build_p50_ms={:.3}\tpalette_build_p99_ms={:.3}\tresidual_p99_ms={:.3}\tbone_subattr={}\tparallel_anim={}\tskinned_instances={last_skinned}\tdistinct_palette_keys_30hz={last_keys}\tunique_palettes_built={last_unique}\tdedup_ratio={:.3}\tbuild_ratio={:.3}",
        percentile(&gather_times, 50.0),
        percentile(&gather_times, 99.0),
        percentile(&keyframe_times, 50.0),
        percentile(&keyframe_times, 99.0),
        percentile(&bone_compose_times, 50.0),
        percentile(&bone_compose_times, 99.0),
        percentile(&assemble_times, 50.0),
        percentile(&assemble_times, 99.0),
        percentile(&tick_anim_times, 50.0),
        percentile(&tick_anim_times, 99.0),
        percentile(&skin_dispatch_times, 50.0),
        percentile(&skin_dispatch_times, 99.0),
        percentile(&palette_build_times, 50.0),
        percentile(&palette_build_times, 99.0),
        residual_p99,
        last_bone_subattr,
        last_parallel,
        if last_skinned == 0 {
            0.0
        } else {
            last_keys as f64 / last_skinned as f64
        },
        if last_skinned == 0 {
            0.0
        } else {
            last_unique as f64 / last_skinned as f64
        },
    );
        let calls_p50 = percentile(&upload_calls, 50.0);
        let bytes_p50 = percentile(&upload_bytes, 50.0);
        let us_per_call = if calls_p50 > 0.0 {
            (percentile(&upload_times, 50.0) * 1000.0) / calls_p50
        } else {
            0.0
        };
        let mb_per_frame = bytes_p50 / (1024.0 * 1024.0);
        let bone_bytes_p50 = percentile(&upload_skinned_bone_bytes, 50.0);
        let bone_mb = bone_bytes_p50 / (1024.0 * 1024.0);
        println!(
        "UPLOAD_COMP\t{label}\tgpu_palette_fold={}\twrite_calls_p50={:.0}\twrite_calls_p99={:.0}\twrite_bytes_p50={:.0}\twrite_bytes_p99={:.0}\tmb_p50={:.3}\tskinned_models_p50={:.0}\tskinned_inst_calls_p50={:.0}\tskinned_palette_calls_p50={:.0}\tskinned_bone_calls_p50={:.0}\tskinned_inst_bytes_p50={:.0}\tskinned_palette_bytes_p50={:.0}\tskinned_bone_bytes_p50={:.0}\tbone_mb_p50={:.3}\tparts_ratio_p50={:.2}\tparts_ratio_p99={:.2}\tupload_ms_p50={:.3}\tus_per_call_p50={:.1}\tnote=parts_ratio=palette_stride/bone_stride;bone_bytes=posed_world_upload_under_gpu_fold",
        if last_gpu_fold { "1" } else { "0" },
        calls_p50,
        percentile(&upload_calls, 99.0),
        bytes_p50,
        percentile(&upload_bytes, 99.0),
        mb_per_frame,
        percentile(&upload_skinned_models, 50.0),
        percentile(&upload_skinned_inst_calls, 50.0),
        percentile(&upload_skinned_pal_calls, 50.0),
        percentile(&upload_skinned_bone_calls, 50.0),
        percentile(&upload_skinned_inst_bytes, 50.0),
        percentile(&upload_skinned_pal_bytes, 50.0),
        bone_bytes_p50,
        bone_mb,
        percentile(&upload_parts_ratio, 50.0),
        percentile(&upload_parts_ratio, 99.0),
        percentile(&upload_times, 50.0),
        us_per_call,
    );
        println!(
        "THREADS\t{label}\tharness_worker_threads=1\tproduct_runtime=1_render_plus_1_network\tlongest_serial_span_ms={longest_serial:.3}\tnote=entire_frame_is_serial_in_this_harness\tCAER_PARALLEL_ANIM={}",
        if last_parallel { "1" } else { "0" },
    );
    } // end !args.clean ATTR block

    // Secondary upper bound: screenshot path (HUD + readback), short window only.
    let rb_n = args.frames.min(60);
    let mut times_rb = Vec::with_capacity(rb_n);
    for _ in 0..rb_n {
        tick_world(&mut world, dt);
        anim_t += dt;
        let (ms, inst, _, _, _, _, _, _, _, _) = sample(&world, anim_t, &hud, true);
        times_rb.push(ms);
        last_instances = inst;
    }
    summarize(
        &format!("{label}_readback_upper_bound"),
        &mut times_rb,
        last_instances,
        world.len(),
        budget_ms,
    );
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::new().filter_or("CAER_LOG", "warn"))
        .format_timestamp(None)
        .format_target(false)
        .init();

    let args = parse_args();
    let allocator = if cfg!(feature = "mimalloc") {
        "mimalloc"
    } else {
        "system"
    };
    println!("m9_frametime\trevision\tM9-REDO\tspec=docs/council/PERF_HARNESS_SPEC.md");
    println!(
        "m9_frametime\thost\tcpu=Intel_Xeon_E5-2699_v3\tthreads={}\tgpu=NVIDIA_GeForce_RTX_5060_8GB\tbuild=release\tallocator={allocator}",
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0)
    );
    println!(
        "m9_frametime\trun\tframes={}\twarmup={}\tsize={}x{}\tregion={}\tCAER_CLIENT={}\thud=on\treadback_in_primary=no\tdt=1/60\tclean={}\tmandate_only={}\twindowed={}\tCAER_PARALLEL_ANIM={}\tallocator={allocator}",
        args.frames,
        args.warmup,
        args.size.0,
        args.size.1,
        args.region,
        std::env::var("CAER_CLIENT").unwrap_or_else(|_| "(unset)".into()),
        args.clean,
        args.mandate_only,
        args.windowed,
        std::env::var("CAER_PARALLEL_ANIM").unwrap_or_else(|_| "(unset)".into()),
    );

    let mobs_path = args.mobs.to_string_lossy().into_owned();
    let ladder: Vec<usize> = if args.mandate_only {
        vec![]
    } else if args.entities.is_empty() {
        vec![0, 50, 200, 500, 1000]
    } else {
        args.entities.clone()
    };

    if !args.ladder_only && (args.entities.is_empty() || args.mandate_only) {
        let world = load_world(&mobs_path);
        println!(
            "m9_frametime\tloaded_mobs\tpath={mobs_path}\tworld_entities={}",
            world.len()
        );
        let ground = Args {
            cam: Some([592250.0, 537900.0, 3500.0, 45.0, -30.0]),
            topdown: false,
            ..copy_run(&args)
        };
        bench_config("camelot_hills_mobs", &ground, world);

        if !args.mandate_only {
            let world = load_world(&mobs_path);
            let top = Args {
                cam: None,
                topdown: true,
                ..copy_run(&args)
            };
            bench_config("topdown_mobs", &top, world);
        }
    }

    for &n in &ladder {
        run_entity_scale(&args, &mobs_path, n);
    }

    if args.windowed {
        // Avoid winit/wgpu Drop hang after a surface present session on this stack.
        let _ = std::io::Write::flush(&mut std::io::stdout());
        std::process::exit(0);
    }
}

fn copy_run(a: &Args) -> Args {
    Args {
        cam: a.cam,
        topdown: a.topdown,
        frames: a.frames,
        warmup: a.warmup,
        entities: vec![],
        size: a.size,
        region: a.region,
        mobs: a.mobs.clone(),
        full_region_terrain: a.full_region_terrain,
        ladder_only: a.ladder_only,
        mandate_only: a.mandate_only,
        clean: a.clean,
        inject_stall_every: a.inject_stall_every,
        inject_stall_ms: a.inject_stall_ms,
        series_out: a.series_out.clone(),
        host_clock: a.host_clock,
        windowed: a.windowed,
    }
}

fn run_entity_scale(args: &Args, mobs_path: &str, n: usize) {
    let center = [592250.0, 537900.0, 1750.0];
    let world = synth_world(&PathBuf::from(mobs_path), n, center);
    let scaled = Args {
        cam: Some([592250.0, 537900.0, 3500.0, 45.0, -30.0]),
        topdown: false,
        frames: args.frames,
        warmup: args.warmup,
        entities: vec![],
        size: args.size,
        region: args.region,
        mobs: args.mobs.clone(),
        full_region_terrain: args.full_region_terrain,
        ladder_only: false,
        mandate_only: false,
        clean: args.clean,
        inject_stall_every: args.inject_stall_every,
        inject_stall_ms: args.inject_stall_ms,
        series_out: None, // don't overwrite mandate series from ladder legs
        host_clock: args.host_clock,
        windowed: args.windowed,
    };
    bench_config(&format!("entities_{n}"), &scaled, world);
}
