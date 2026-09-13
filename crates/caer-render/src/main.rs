//! caer-render — the placeholder renderer.
//!
//! Opens a wgpu window and draws the live `caer-world` `WorldState` as instanced boxes at their
//! real DAoC world positions, with a free-fly camera. Every frame it asks the world model for
//! the render set around the camera (`render_set_par` — the grid-culled, parallel per-frame pass
//! we validated at ~100x cull / 4.7x parallel on the real 16k-mob Camelot Hills population) and
//! draws exactly that near slice. Fly out and the culling visibly thins the field; that is the
//! substrate working, on screen.
//!
//! This is P3-placeholder: boxes, no meshes/terrain/textures yet. Those slot in behind the same
//! world→render seam later (P2 asset pipeline). Run:
//!
//!   cargo run --release -p caer-render -- [mobs.tsv]
//!
//! Defaults to `captures/mobs_region1.tsv` (the 16,419-spawn Camelot Hills dump).
//!
//! Controls: WASD move · Space/C up-down · Shift boost · mousewheel speed · right-drag look · left-drag pan · Esc quit.

use std::io::Write;

use std::time::Instant;

use glam::Vec3;
use winit::application::ApplicationHandler;
use winit::event::{
    DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, WindowId};

use caer_world::{WorldState, ZONE_UNIT};

use caer_render::camera::{Camera, Movement};
use caer_render::gpu::{Gpu, Instance};
use caer_render::shell::Shell;
use caer_render::{entities, live, terrain, ui};
use caer_render::{
    fixture_instances, load_world, render_world, scene_bounds, world_bbox, CULL_RADIUS, LIVE_WARMUP,
};

/// World units a single WASD/Space/Z press moves the selected fixture in the editor.
const MOVE_STEP: f32 = 5.0;

/// Parsed CLI: `[mobs.tsv] [--screenshot out.png] [--topdown] [--cam x,y,z,yaw,pitch] [--size WxH]`.
/// yaw/pitch in degrees, x/y/z in absolute DAoC world units.
struct Args {
    path: String,
    /// Whether `path` was explicitly given (vs the Albion default). A non-default region with
    /// no explicit dump renders the whole region, unpopulated.
    path_explicit: bool,
    screenshot: Option<String>,
    topdown: bool,
    cam: Option<[f32; 5]>,
    size: (u32, u32),
    /// Which server region's zones to load terrain for (offsets only make sense per region).
    region: u16,
    /// Interactive mode: pop the zone atlas viewer alongside the render window. Off by default now
    /// that the current zone shows in the title bar; opt in with `--atlas`.
    atlas: bool,
    /// `--live`: feed the `WorldState` from a real DoL server instead of a static mob dump.
    live: Option<LiveArgs>,
    /// `--avatars race:gender,…`: spawn assembled player-avatar bodies in a row for headless
    /// inspection. The avatar is normally only reachable through a live login, which made it the
    /// one Phase-A mesh no screenshot could check; this makes it verifiable like any creature.
    avatars: Vec<(u8, u8)>,
    /// Where to place that row (absolute world XYZ). Defaults near the Camelot Hills test spot.
    avatars_at: [f32; 3],
    /// `--ui-window <name>`: draw one skin window through the UI pipeline (GPU check).
    ui_window: Option<String>,
    /// `--avatar-heading N`: spawn the avatars at this DAoC heading (0..4096) instead of 0, so the
    /// mapping from wire heading to on-screen facing can be checked against a KNOWN value rather
    /// than eyeballed. Without a controlled reference, "which way is it facing" is unanswerable.
    avatar_heading: u16,
    /// `--avatar-speed N`: spawn the avatars moving at N units/s so the LOCOMOTION clip plays
    /// instead of idle. Animation bugs that only show while walking (a joint that separates as the
    /// limb swings) are invisible at idle, which is otherwise the only pose this viewer can show.
    avatar_speed: u16,
}

/// Connection parameters for `--live`.
struct LiveArgs {
    server: String,
    account: String,
    password: String,
    character: Option<String>,
}

/// Load a skin, lay one window out near the top-left, upload its atlas pages, and queue its quads.
fn draw_skin_window(gpu: &mut Gpu, window: &str) {
    let ui_dir = terrain::client_root().join("ui");
    let skin = match caer_assets::uiskin::Skin::load(&ui_dir, "atlantis") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("caer-render: skin load failed: {e}");
            return;
        }
    };
    let Some(w) = skin.windows.get(window) else {
        eprintln!("caer-render: no window `{window}`");
        return;
    };
    let draw =
        caer_render::skinui::layout_window(&skin, w, (40.0, 40.0), &|a| Some(format!("<{a}>")));
    // Texture `File` paths are relative to `ui/`, not to the skin directory.
    for q in &draw.quads {
        if gpu.has_ui_page(&q.texture) {
            continue;
        }
        let Some(tex) = skin.texture(&q.texture) else {
            continue;
        };
        let tex_path = caer_assets::uiskin::resolve_ignoring_case(&ui_dir, &tex.file);
        match std::fs::read(&tex_path).map(|b| caer_assets::tga::decode(&b)) {
            Ok(Ok(img)) => gpu.upload_ui_page(&q.texture, &img),
            _ => eprintln!("caer-render: could not load UI page {}", tex.file),
        }
    }
    // Text: load each referenced bitmap font, upload its atlas as a UI page, and append the glyph
    // quads AFTER the frame so they land on top of it (painter's order).
    let mut quads = draw.quads.clone();
    let mut fonts: std::collections::HashMap<String, caer_assets::bitmapfont::BitmapFont> =
        Default::default();
    for t in &draw.texts {
        let Some(fname) = t.font.as_deref() else {
            continue;
        };
        let key = fname.to_ascii_lowercase();
        if !fonts.contains_key(&key) {
            let Some(decl) = skin.font(fname) else {
                continue;
            };
            // TrueType fonts are a separate path; only the bitmap atlases load here.
            if decl.file.to_ascii_lowercase().ends_with(".ttf") {
                continue;
            }
            let font_path = caer_assets::uiskin::resolve_ignoring_case(&ui_dir, &decl.file);
            let Ok(bytes) = std::fs::read(&font_path) else {
                eprintln!("caer-render: could not read font {}", decl.file);
                continue;
            };
            let Ok(img) = caer_assets::tga::decode(&bytes) else {
                continue;
            };
            let Some(f) = caer_assets::bitmapfont::BitmapFont::parse(img) else {
                eprintln!("caer-render: {} is not a bitmap font atlas", decl.file);
                continue;
            };
            gpu.upload_ui_page(&key, &f.atlas);
            fonts.insert(key.clone(), f);
        }
        if let Some(f) = fonts.get(&key) {
            caer_render::skinui::text_quads(f, &key, t, &mut quads);
        }
    }
    gpu.set_ui_quads(&quads);
    println!(
        "caer-render: skin window `{window}` -> {} frame quads + {} glyph quads, {} font(s)",
        draw.quads.len(),
        quads.len() - draw.quads.len(),
        fonts.len(),
    );
}

fn parse_args() -> Args {
    let mut args = Args {
        path: "captures/mobs_region1.tsv".to_string(),
        path_explicit: false,
        screenshot: None,
        topdown: false,
        cam: None,
        size: (1600, 1000),
        region: 1,
        atlas: false,
        live: None,
        avatars: Vec::new(),
        ui_window: None,
        avatars_at: [592250.0, 537900.0, 1750.0],
        avatar_speed: 0,
        avatar_heading: 0,
    };
    // `--live` collects its connection flags separately, then folds into args.live at the end.
    let mut live_on = false;
    let mut live_server = "127.0.0.1:10311".to_string();
    let mut live_account = "rustdaoc".to_string();
    let mut live_password = "rustdaoc".to_string();
    let mut live_char: Option<String> = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--screenshot" => args.screenshot = it.next(),
            "--topdown" => args.topdown = true,
            "--cam" => {
                let v: Vec<f32> = it
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .filter_map(|s| s.trim().parse().ok())
                    .collect();
                if v.len() == 5 {
                    args.cam = Some([v[0], v[1], v[2], v[3], v[4]]);
                } else {
                    eprintln!("caer-render: --cam wants x,y,z,yaw,pitch (got {a:?})");
                    std::process::exit(2);
                }
            }
            "--region" => args.region = it.next().and_then(|s| s.parse().ok()).unwrap_or(1),
            "--live" => live_on = true,
            "--server" => live_server = it.next().unwrap_or(live_server),
            "--account" => live_account = it.next().unwrap_or(live_account),
            "--password" => live_password = it.next().unwrap_or(live_password),
            "--char" => live_char = it.next(),
            "--avatars" => {
                // `race:gender` pairs, e.g. `1:1,1:2,5:1` = Briton M/F + Norse M.
                args.avatars = it
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .filter_map(|p| {
                        let (r, g) = p.trim().split_once(':')?;
                        Some((r.trim().parse().ok()?, g.trim().parse().ok()?))
                    })
                    .collect();
            }
            "--avatar-heading" => {
                args.avatar_heading = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            }
            "--avatar-speed" => {
                args.avatar_speed = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            }
            "--avatars-at" => {
                let v: Vec<f32> = it
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .filter_map(|s| s.trim().parse().ok())
                    .collect();
                if v.len() == 3 {
                    args.avatars_at = [v[0], v[1], v[2]];
                }
            }
            "--ui-window" => args.ui_window = it.next(),
            "--atlas" => args.atlas = true,
            "--no-atlas" => {} // accepted no-op: atlas is now off by default (kept for old scripts)
            "--size" => {
                if let Some((w, h)) = it.next().unwrap_or_default().split_once('x') {
                    args.size = (w.parse().unwrap_or(1600), h.parse().unwrap_or(1000));
                }
            }
            other => {
                args.path = other.to_string();
                args.path_explicit = true;
            }
        }
    }
    if live_on {
        args.live = Some(LiveArgs {
            server: live_server,
            account: live_account,
            password: live_password,
            character: live_char,
        });
    }
    args
}

/// Initialise diagnostics. Library crates write to the `log` facade; this is the sink.
///
/// Defaults differ by binary on purpose: the product client is quiet (`warn`) so a launch shows
/// only things that are wrong, while the dev viewer/tools default to `info`. Override with
/// `CAER_LOG` using standard env_logger syntax (`CAER_LOG=debug`, `CAER_LOG=caer_render=debug`).
/// The default filter is scoped to OUR crates so wgpu/winit info-chatter stays out of a normal run.
fn init_logging(default: &str) {
    env_logger::Builder::from_env(env_logger::Env::new().filter_or("CAER_LOG", default))
        .format_timestamp(None)
        .format_target(false)
        .init();
}

/// Clip time for a headless `--screenshot` frame. Fixed (0.0) by default so golden images render
/// the same pose every run; override with `CAER_ANIM_TIME=<seconds>` to capture a different frame
/// of the animation, which is how skinning is verified headlessly without a window.
fn screenshot_anim_time() -> f32 {
    std::env::var("CAER_ANIM_TIME")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0)
}

fn main() {
    init_logging("warn,caer_render=info,caer_assets=info,caer_client=info");
    let args = parse_args();

    // `--probe x,y [region]`: print a 7×7 grid of raw terrain heights (2,000-unit spacing) around
    // the point, then exit. Used to find a valley-floor elevation to author a flatten patch against.
    let raw: Vec<String> = std::env::args().collect();
    if let Some(i) = raw.iter().position(|a| a == "--probe") {
        let center: Vec<i32> = raw
            .get(i + 1)
            .map(|s| s.split(',').filter_map(|v| v.trim().parse().ok()).collect())
            .unwrap_or_default();
        let region = raw.get(i + 2).and_then(|s| s.parse().ok()).unwrap_or(1);
        if center.len() == 2 {
            let (cx, cy) = (center[0], center[1]);
            println!("caer-render: terrain heights around ({cx}, {cy}) region {region}, 2000-unit grid (rows = +Y north→south):");
            for dy in (-6000..=6000).step_by(2000) {
                let row: Vec<String> = (-6000..=6000)
                    .step_by(2000)
                    .map(|dx| {
                        terrain::probe_height(region, cx + dx, cy + dy)
                            .map_or_else(|| "  --  ".into(), |h| format!("{h:6.0}"))
                    })
                    .collect();
                println!("  y{:+6}: {}", dy, row.join(" "));
            }
        } else {
            eprintln!("usage: --probe x,y [region]");
        }
        return;
    }

    // `--export-heightmap PATH.png` / `--verify-heightmap PATH.png`: the UE5-Landscape round-trip
    // bridge (see caer_assets::heightmap). Export stitches the region's PRISTINE terrain (pre any
    // flatten patch) into a 16-bit PNG + sidecar for sculpting; verify additionally reads it back
    // and reports reconstruction error vs the source — the Slice-0 proof on real data. Handled
    // here, before the mob dump loads: terrain heights are origin-independent, so no world needed.
    if let Some(i) = raw
        .iter()
        .position(|a| a == "--export-heightmap" || a == "--verify-heightmap")
    {
        let verify = raw[i] == "--verify-heightmap";
        let out = raw
            .get(i + 1)
            .filter(|s| !s.starts_with("--"))
            .cloned()
            .unwrap_or_else(|| format!("captures/region{}_heightmap.png", args.region));
        // Wide bounds → every zone in the region; zero origin (heights are origin-independent).
        let full = (i32::MIN / 2, i32::MAX / 2);
        let mesh = terrain::load_region(
            args.region,
            Vec3::ZERO,
            [full.0, full.0],
            [full.1, full.1],
            terrain::seam_blend(),
        );
        if mesh.heights_raw.is_empty() {
            eprintln!(
                "caer-render: no terrain decoded for region {} — nothing to export",
                args.region
            );
            std::process::exit(2);
        }
        let hm = caer_assets::heightmap::export(&mesh.heights_raw, args.region);
        println!(
            "caer-render: heightmap — region {} · {} zones · data {}×{} → padded {}×{} (UE) · Z [{:.1}, {:.1}]",
            args.region, hm.zones.len(), hm.data_w, hm.data_h, hm.pad_w, hm.pad_h, hm.min_z, hm.max_z,
        );
        if let Err(e) = caer_assets::heightmap::write_png(&hm, &out) {
            eprintln!("caer-render: FAILED to write heightmap {out}: {e}");
            std::process::exit(1);
        }
        println!("caer-render: wrote {out} (+ sidecar .txt)");
        if verify {
            let rd = caer_assets::heightmap::read_png(&out).unwrap_or_else(|e| {
                eprintln!("caer-render: FAILED to read back heightmap {out}: {e}");
                std::process::exit(1);
            });
            let back = caer_assets::heightmap::import(&rd);
            let (mut worst, mut sum, mut n) = (0.0f32, 0.0f64, 0u64);
            for (k, orig) in &mesh.heights_raw {
                if let Some(got) = back.get(k) {
                    for s in 0..orig.heights.len() {
                        let d = (orig.heights[s] - got.heights[s]).abs();
                        worst = worst.max(d);
                        sum += d as f64;
                        n += 1;
                    }
                }
            }
            let tol = (hm.max_z - hm.min_z) / 65535.0 * 1.5;
            let mean = if n > 0 { sum / n as f64 } else { 0.0 };
            println!(
                "caer-render: round-trip over {n} samples — worst {worst:.3} u, mean {mean:.4} u, quant tol {tol:.3} u → {}",
                if worst <= tol { "PASS (within one quantization step)" } else { "SEAM DELTA present (overlapping seam samples unified — expected)" },
            );
        }
        return;
    }

    let path = args.path.clone();

    // `--live`: feed the world from a real DoL server. Spawn the session on a background thread,
    // then WARM UP — drain events for a short window so the initial population + spawn position
    // are present before we size the scene and open the window. The feed keeps streaming after.
    let (world, origin, extent, live_feed) = if let Some(la) = &args.live {
        let feed = live::spawn(
            la.server.clone(),
            la.account.clone(),
            la.password.clone(),
            la.character.clone(),
            true, // inspection lens: auto-select is fine
        );
        let mut world = WorldState::new();
        let mut player: Option<[f32; 3]> = None;
        println!(
            "caer-render: --live — warming up from {} (up to {}s)…",
            la.server,
            LIVE_WARMUP.as_secs()
        );
        let start = Instant::now();
        // Warm up until the spawn position lands (we're in-world with a centre) or the window ends.
        while start.elapsed() < LIVE_WARMUP {
            let d = live::drain_into(feed.events(), &mut world);
            if let Some(pp) = d.player {
                player = Some(pp);
            }
            // Once we have the spawn point and some population, we've seen enough to size the scene.
            if player.is_some() && world.len() > 4 && d.applied == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // Centre the render origin on the character if we have it, else on the population/region.
        let (origin, extent) = match player {
            Some([x, y, _]) => (
                Vec3::new(x, y, screenshot_anim_time()),
                scene_bounds(&world, args.region).1,
            ),
            None => scene_bounds(&world, args.region),
        };
        println!(
            "caer-render: live warm-up done — {} entities, player {:?}",
            world.len(),
            player.map(|p| [p[0], p[1], p[2]])
        );
        (world, origin, extent, Some(feed))
    } else {
        // A non-default region without its own dump: render the whole region, no population.
        let world = if args.region != 1 && !args.path_explicit {
            WorldState::new()
        } else {
            load_world(&path)
        };
        let (origin, extent) = scene_bounds(&world, args.region);
        (world, origin, extent, None)
    };

    if args.screenshot.is_some() {
        return screenshot(&args, world, origin, extent);
    }
    println!(
        "caer-render: {} entities loaded from {path}\n  origin {:?}  cull radius {CULL_RADIUS}\n  \
         WASD move · Space/C up/down · Shift boost · hold right-mouse to look · Esc quit",
        world.len(),
        origin.to_array(),
    );
    std::io::stdout().flush().ok();

    // Pop the zone atlas next to the render window only when asked (regenerate with atlas.sh).
    if args.atlas {
        let atlas = if args.region == 1 {
            "captures/albion_atlas.png".to_string()
        } else {
            format!("captures/atlas_region{}.png", args.region)
        };
        if std::path::Path::new(&atlas).exists() {
            let _ = std::process::Command::new("xdg-open")
                .arg(&atlas)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }

    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new(world, origin, extent, args.region, path.clone());
    // The App owns the whole feed (channel + thread handle), so the session thread lives exactly
    // as long as the window; the render loop drains `app.live` each frame.
    app.live = live_feed;
    event_loop.run_app(&mut app).expect("run app");
}

/// Headless mode: render exactly one frame to a PNG and exit. This is the agent-facing debug
/// loop — edit terrain/render code, `--screenshot`, Read the PNG, repeat, no window or human
/// needed. `--topdown` gives a plan view of the whole loaded region (layout/seam debugging);
/// `--cam x,y,z,yaw,pitch` reproduces any in-game vantage.
fn screenshot(args: &Args, mut world: WorldState, origin: Vec3, extent: f32) {
    let out = args.screenshot.as_deref().unwrap();
    let (w, h) = args.size;
    let mut gpu = pollster::block_on(Gpu::new_headless(w, h, extent)).unwrap_or_else(|e| {
        eprintln!("caer-render: headless GPU init failed:\n{e}");
        std::process::exit(1);
    });

    let (min, max) = world_bbox(&world, args.region);
    let mesh = terrain::load_region(args.region, origin, min, max, terrain::seam_blend());
    let (nv, nt) = mesh.totals();
    println!(
        "caer-render: terrain — {} zones, {nv} verts / {nt} tris; {} model kinds ({} placed), {} box fixtures",
        mesh.zones_loaded,
        mesh.models.len(),
        mesh.models.iter().map(|m| m.instances.len()).sum::<usize>(),
        mesh.fixtures.len(),
    );
    gpu.set_terrain(&mesh.zones);
    gpu.set_water(&mesh.water_vertices, &mesh.water_indices);
    gpu.set_models(&mesh.models, &mesh.textures);
    gpu.set_fixtures(&fixture_instances(&mesh.fixtures));

    let mut camera = if let Some([x, y, z, yaw, pitch]) = args.cam {
        let mut c = Camera::new(Vec3::new(x, y, z), origin, origin, gpu.aspect());
        c.set_look_deg(yaw, pitch);
        c
    } else if args.topdown {
        // High plan view over the region's centre, looking straight down (+X right, +Y up-screen).
        let cx = (min[0] + max[0]) as f32 / 2.0;
        let cy = (min[1] + max[1]) as f32 / 2.0;
        let span = ((max[0] - min[0]).max(max[1] - min[1])) as f32;
        // Camera height must clear the SCENE's top, not just origin.z: a tight cluster sitting high
        // up (e.g. a dungeon at z≈6000) has a small XY span, so `span*0.9` alone would place the
        // camera *below* the entities and look at empty sky. Lift above the highest entity Z.
        let top_z = world.positions().iter().map(|p| p[2]).max().unwrap_or(0) as f32;
        let height = span.mul_add(0.9, top_z);
        let mut c = Camera::new(Vec3::new(cx, cy, height), origin, origin, gpu.aspect());
        c.set_look_deg(90.0, -88.0);
        c.ensure_far(height * 1.5);
        c
    } else {
        let r = CULL_RADIUS as f32;
        Camera::new(
            origin + Vec3::new(0.0, -0.8 * r, 0.7 * r),
            origin,
            origin,
            gpu.aspect(),
        )
    };
    camera.set_aspect(w as f32 / h as f32);

    // Same per-frame path as the interactive loop, once. Topdown culls from the region centre
    // with a radius covering everything, so the plan view shows the full population.
    let cull_radius = if args.topdown {
        i32::MAX / 4
    } else {
        CULL_RADIUS
    };
    // Resolve entity model ids to NIF meshes for this single frame, exactly like the live loop.
    let mut entity_models = entities::EntityModels::load();
    // `--avatars`: assemble each requested player body and drop it into the world as an entity, so
    // the avatar mesh renders through the same instanced path as any creature.
    if !args.avatars.is_empty() {
        if let Some(em) = entity_models.as_mut() {
            spawn_avatars(args, &mut world, em, &mut gpu);
        }
    }
    let mut mesh_instances: std::collections::HashMap<u16, Vec<[f32; 5]>> =
        std::collections::HashMap::new();
    let mut instances = Vec::new();
    let count = render_world(
        &world,
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
        0.0,
        screenshot_anim_time(),
    );
    gpu.set_view_proj(camera.view_proj().to_cols_array_2d());
    gpu.upload_instances(&instances);
    // `--ui-window <name>`: draw a real skin window through the UI pipeline, over the world. This
    // is the GPU-side check for the layout stage — the CPU composite in `uilayout` proves the
    // geometry, so a difference here is a binding or blend problem rather than maths.
    if let Some(win) = &args.ui_window {
        draw_skin_window(&mut gpu, win);
    }
    let rgba = gpu.render_to_rgba(count).expect("GPU wait");

    let file = std::fs::File::create(out).unwrap_or_else(|e| {
        eprintln!("caer-render: cannot create {out}: {e}");
        std::process::exit(1);
    });
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .expect("png header")
        .write_image_data(&rgba)
        .expect("png data");
    println!(
        "caer-render: screenshot {out} ({w}x{h}, {} instances, cam {:?})",
        instances.len(),
        camera.world_pos.to_array(),
    );
}

/// Assemble each `--avatars race:gender` body, upload it, and place it in the world in a row so a
/// single screenshot shows them side by side. The avatar uses a synthetic model id
/// ([`entities::avatar_model_id`]) that the entity renderer already treats like any other mesh, so
/// nothing special happens downstream — this only injects the spawns a live login would provide.
fn spawn_avatars(
    args: &Args,
    world: &mut WorldState,
    models: &mut entities::EntityModels,
    gpu: &mut Gpu,
) {
    use caer_protocol::entities::Npc;
    use caer_protocol::session::ServerEvent;
    const SPACING: f32 = 120.0;
    let [ax, ay, az] = args.avatars_at;
    // Keep clear of the object-id range the mob dump already used.
    let mut next_id: u16 = 50_000;
    for (i, &(race, gender)) in args.avatars.iter().enumerate() {
        let Some(model) = models.ensure_avatar(
            gpu,
            race,
            gender,
            caer_protocol::customization::Customization::default(),
            None,
        ) else {
            eprintln!("caer-render: --avatars — race {race} gender {gender} did not resolve");
            continue;
        };
        let x = ax + i as f32 * SPACING;
        world.apply(&ServerEvent::NpcInView(Npc {
            object_id: next_id,
            speed: args.avatar_speed,
            heading: args.avatar_heading,
            x: x.max(0.0) as u32,
            y: ay.max(0.0) as u32,
            z: az.clamp(0.0, f32::from(u16::MAX)) as u16,
            model,
            size: 50,
            level: 1,
            flags: 0,
            name: format!("avatar r{race}g{gender}"),
            guild: String::new(),
        }));
        next_id = next_id.wrapping_add(1);
    }
}

/// A flatten patch's full world-space footprint (radius + falloff), as a dirty rect for the
/// zone-restricted re-mesh.
fn patch_rect(p: &terrain::FlattenRegion) -> [f32; 4] {
    let r = p.radius + p.falloff;
    [p.cx - r, p.cy - r, p.cx + r, p.cy + r]
}

/// One placed model the editor can select and re-orient. Extracted from the loaded mesh so the
/// app keeps only what picking needs (a bounding sphere + identity), not the full geometry.
struct EditInstance {
    /// Index of the source `ModelBatch` (mesh `models` order) and the instance within it — the
    /// address the GPU live-update uses.
    batch: usize,
    inst: usize,
    /// Model-space offset of the bounding-sphere centre (the mesh AABB centre × scale) from
    /// `pos`. Rotated by the instance yaw at pick time — baked-coordinate models (Krondon's
    /// stronghold quads, SI glacier walls) put their mesh tens of thousands of units from the
    /// fixture origin, so an XY-less offset made them unclickable.
    center_off: Vec3,
    radius: f32,
    /// Current render-space position + the CSV baseline (for revert / move-delta).
    pos: [f32; 3],
    base_pos: [f32; 3],
    scale: f32,
    /// Current effective yaw and the CSV baseline (for revert), radians.
    yaw: f32,
    base_yaw: f32,
    /// The authored rotation to restore on revert. A fixture whose axis is not ±Z cannot be
    /// described by `base_yaw`, so reverting from that alone would flatten its authored tilt.
    base_rot: [f32; 4],
    /// Stable identity for persisting the override.
    zone_id: u16,
    fixture_id: u32,
}

impl EditInstance {
    /// Render-space bounding-sphere centre: the model-space AABB centre rotated by the
    /// instance yaw, tracking the model's current position + orientation.
    fn center(&self) -> Vec3 {
        let (s, c) = self.yaw.sin_cos();
        let o = self.center_off;
        Vec3::from(self.pos) + Vec3::new(c * o.x - s * o.y, s * o.x + c * o.y, o.z)
    }
}

/// Per-batch geometry the editor keeps for exact click-picking: model-space triangles + AABB.
/// The bounding-sphere test alone was unusable in clutter — a 2,000-unit elm's sphere swallows
/// the building beside it, so clicks "right on" a house kept selecting trees. Spheres are now
/// only the broad phase; the actual hit is ray-vs-triangle.
struct PickMesh {
    positions: Vec<[f32; 3]>,
    indices: Vec<u32>,
    min: [f32; 3],
    max: [f32; 3],
}

/// Möller–Trumbore ray/triangle intersection. Returns the distance `t` along the (unit) ray,
/// or None on miss. Culling is off — NIF winding varies and the meshes render double-sided.
fn ray_triangle(ro: Vec3, rd: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let e1 = b - a;
    let e2 = c - a;
    let p = rd.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-8 {
        return None;
    }
    let inv = 1.0 / det;
    let s = ro - a;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = rd.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t > 0.0).then_some(t)
}

/// The in-renderer fixture editor: toggle on (Tab), click a model, then transform it —
/// Q/E rotate ±15° (Alt = ±5°), ,/. nudge ±1°, WASD move N/S/E/W, Space/Z raise/lower,
/// R revert. Edits auto-save to a tracked TSV and re-apply on every load. The first tool of a
/// RustDAoC world-editing toolchain (model-swap / terrain-smoothing next).
#[derive(Default)]
struct Editor {
    on: bool,
    /// Whether an Alt key is currently held (fine-rotation modifier).
    alt: bool,
    /// Whether a Ctrl key is currently held (Ctrl+S = manual save).
    ctrl: bool,
    instances: Vec<EditInstance>,
    /// Model-space triangle geometry per batch (mesh `models` order), for exact picking and
    /// the selection outline box.
    pick_meshes: Vec<PickMesh>,
    selected: Option<usize>,
    overrides: terrain::FixtureOverrides,
    cursor: (f32, f32),
}

struct App {
    world: WorldState,
    origin: Vec3,
    extent: f32,
    region: u16,
    /// Path to the region-1 mob dump, so switching back to Albion re-populates it (other regions
    /// render unpopulated — no dump).
    mob_path: String,
    /// Window + GPU surface + camera lifecycle, shared with the player client (`shell::Shell`).
    shell: Shell,
    movement: Movement,
    looking: bool,
    panning: bool,
    editor: Editor,
    /// egui sidebar (created with the window) + the state it renders from.
    ui: Option<ui::Ui>,
    sidebar: ui::SidebarState,
    /// Retained for terrain reloads (seam-blend slider): loader bounds.
    world_min: [i32; 2],
    world_max: [i32; 2],
    /// Raw decoded zone heightmaps from the last terrain load, keyed by zone grid offset —
    /// the terrain brush ray-marches and samples against these (no per-click zone decode).
    terrain_heights: std::collections::HashMap<(i32, i32), caer_assets::terrain::ZoneTerrain>,
    /// Pristine decoded heightmaps (no patches) — undo/erase rebuilds work areas from these.
    terrain_raw: std::collections::HashMap<(i32, i32), caer_assets::terrain::ZoneTerrain>,
    /// Drag painting: last stamp time + world position, so hold-and-sweep re-stamps at a
    /// sensible cadence instead of every frame.
    last_stamp: Option<(Instant, [f32; 2])>,
    brush_dragging: bool,
    /// Flatten mode's reference elevation, captured at the START of a stroke and held for its
    /// whole duration — so dragging the cursor over humps/dips levels them all to the elevation
    /// where you first pressed (e.g. start on the keep floor, trace the path). Reset on release
    /// (and when the brush is re-pressed) so the next stroke re-captures. `None` = not mid-stroke.
    brush_stroke_target: Option<f32>,
    /// Zone emission recipe from the last load (offset + textured flag, GPU order) — lets a
    /// brush stroke rebuild terrain vertices in-place instead of re-running the full load.
    zone_order: Vec<(i32, i32, bool)>,
    /// All flatten patches (every region) — the SESSION source of truth. The brush edits this
    /// Vec in memory; the TSV store is written on a debounced flush (`flush_flatten`), never
    /// per stamp — a synchronous fuseblk write per stamp was a large slice of the stutter.
    flatten_patches: Vec<terrain::FlattenRegion>,
    /// Unsaved brush edits pending flush, and when the last one landed (the debounce clock).
    flatten_dirty: bool,
    flatten_edited_at: Instant,
    /// Region height-colour scale, computed once per terrain load — `remesh_terrain` used to
    /// resample + sort the whole region's heightfield for this on every stamp.
    color_scale: (f32, f32),
    /// Last zone that answered `sample_height`. Brush samples cluster spatially (ray-march
    /// steps, drape lattice), so checking it first makes the lookup O(1) instead of a scan
    /// over every loaded zone per sample.
    last_sample_zone: std::cell::Cell<Option<(i32, i32)>>,
    /// In-flight background terrain load, if any: (generation, seam blend, finished mesh).
    pending_load: Option<std::sync::mpsc::Receiver<(u64, usize, terrain::TerrainMesh)>>,
    /// Monotonic load generation — a finished load only applies if it's still the latest.
    load_gen: u64,
    /// Reused each frame so the visible-set upload doesn't reallocate.
    instances: Vec<Instance>,
    last_frame: Instant,
    fps_timer: Instant,
    frames: u32,
    logged_first: bool,
    /// Total frames since launch (drives the `$CAER_TEST_SWAP` debug harness).
    frames_total: u64,
    /// `--live`: the background DoL session feeding the world. `Some` → each frame drains new
    /// server events into `self.world` before rendering; `None` → static dump, drawn as loaded.
    live: Option<live::LiveFeed>,
    /// Resolves entity model ids to NIF meshes (lazy per-model upload). `None` if client assets
    /// are absent — entities then render as boxes.
    entity_models: Option<entities::EntityModels>,
    /// Reused each frame: entity mesh instances grouped by model id, so the per-model instance
    /// buffers upload without reallocating.
    mesh_instances: std::collections::HashMap<u16, Vec<[f32; 5]>>,
}

impl App {
    fn new(world: WorldState, origin: Vec3, extent: f32, region: u16, mob_path: String) -> Self {
        let now = Instant::now();
        Self {
            world,
            origin,
            extent,
            region,
            mob_path,
            shell: Shell::new(),
            movement: Movement::default(),
            looking: false,
            panning: false,
            editor: Editor::default(),
            ui: None,
            sidebar: ui::SidebarState::default(),
            world_min: [0, 0],
            world_max: [0, 0],
            terrain_heights: std::collections::HashMap::new(),
            terrain_raw: std::collections::HashMap::new(),
            last_stamp: None,
            brush_dragging: false,
            brush_stroke_target: None,
            zone_order: Vec::new(),
            flatten_patches: Vec::new(),
            flatten_dirty: false,
            flatten_edited_at: now,
            color_scale: (0.0, 4000.0),
            last_sample_zone: std::cell::Cell::new(None),
            instances: Vec::new(),
            last_frame: now,
            fps_timer: now,
            frames: 0,
            logged_first: false,
            frames_total: 0,
            pending_load: None,
            load_gen: 0,
            live: None,
            entity_models: entities::EntityModels::load(),
            mesh_instances: std::collections::HashMap::new(),
        }
    }

    /// Kick off a terrain (re)load on a background thread. The heavy CPU work — zone decode,
    /// NIF parsing, DDS decode — must NOT run inside the winit event handler: a 10–20s stall
    /// there leaves the window unable to answer the compositor, KWin marks it unresponsive
    /// ("map swap freezes"), and the eventual force-close segfaults in egui's Wayland clipboard
    /// teardown. `frame()` polls the channel and applies the finished mesh on arrival; the
    /// generation counter discards a stale result if another load started meanwhile.
    fn load_terrain(&mut self, blend: usize) {
        // The loader replays the flatten TSV — unsaved brush edits must land first.
        self.flush_flatten(true);
        self.load_gen += 1;
        let gen = self.load_gen;
        let (region, origin) = (self.region, self.origin);
        let (min, max) = (self.world_min, self.world_max);
        let (tx, rx) = std::sync::mpsc::channel();
        self.pending_load = Some(rx);
        std::thread::spawn(move || {
            let mesh = terrain::load_region(region, origin, min, max, blend);
            let _ = tx.send((gen, blend, mesh)); // receiver gone = a newer load superseded us
        });
    }

    /// Apply a background-loaded terrain mesh: GPU uploads + editor pick-list rebuild. Fast
    /// (upload only), safe to run inside the event handler.
    fn finish_load(&mut self, blend: usize, mut mesh: terrain::TerrainMesh) {
        self.terrain_heights = std::mem::take(&mut mesh.heights);
        self.terrain_raw = std::mem::take(&mut mesh.heights_raw);
        self.zone_order = std::mem::take(&mut mesh.zone_order);
        // Fixed for the life of this load: brush re-meshes reuse it instead of resampling.
        self.color_scale = terrain::height_color_scale(&self.terrain_heights);
        self.last_sample_zone.set(None);
        let Some(gpu) = self.shell.gpu_mut() else {
            return;
        };
        if mesh.zones_loaded > 0 {
            let (nv, nt) = mesh.totals();
            println!(
                "caer-render: terrain — {} zones, {nv} verts / {nt} tris, {} fixtures (seam blend {blend})",
                mesh.zones_loaded,
                mesh.fixtures.len(),
            );
            std::io::stdout().flush().ok();
        }
        gpu.set_terrain(&mesh.zones);
        gpu.set_water(&mesh.water_vertices, &mesh.water_indices);
        gpu.set_models(&mesh.models, &mesh.textures);
        gpu.set_fixtures(&fixture_instances(&mesh.fixtures));

        // Rebuild the editor pick list (bounding sphere + identity per placed model).
        let mut edit_instances = Vec::new();
        for (bi, b) in mesh.models.iter().enumerate() {
            for (ii, m) in b.instances.iter().enumerate() {
                edit_instances.push(EditInstance {
                    batch: bi,
                    inst: ii,
                    center_off: Vec3::new(
                        (b.bound_min[0] + b.bound_max[0]) * 0.5 * m.scale,
                        (b.bound_min[1] + b.bound_max[1]) * 0.5 * m.scale,
                        (b.bound_min[2] + b.bound_max[2]) * 0.5 * m.scale,
                    ),
                    radius: b.bound_radius * m.scale,
                    pos: m.pos,
                    base_pos: m.base_pos,
                    scale: m.scale,
                    yaw: m.yaw,
                    base_yaw: m.base_yaw,
                    base_rot: m.base_rot,
                    zone_id: m.zone_id,
                    fixture_id: m.fixture_id,
                });
            }
        }
        self.editor.instances = edit_instances;
        self.editor.pick_meshes = mesh
            .models
            .iter()
            .map(|b| PickMesh {
                positions: b.vertices.iter().map(|v| v.pos).collect(),
                indices: b.indices.clone(),
                min: b.bound_min,
                max: b.bound_max,
            })
            .collect();
        self.editor.selected = None;
        self.sync_selection_box();
    }

    /// Switch the loaded map to `region`: reload population (region 1 = the mob dump, others
    /// unpopulated), recompute the scene origin/bounds, re-frame the camera, and reload terrain.
    fn switch_region(&mut self, region: u16) {
        println!("caer-render: switching to region {region}…");
        self.region = region;
        self.world = if region == 1 {
            load_world(&self.mob_path)
        } else {
            WorldState::new()
        };
        let (origin, extent) = scene_bounds(&self.world, region);
        self.origin = origin;
        self.extent = extent;
        let (min, max) = world_bbox(&self.world, region);
        self.world_min = min;
        self.world_max = max;
        // Re-frame the camera over the new region's centre (origin changed, so rebuild it).
        if let Some(gpu) = self.shell.gpu() {
            let r = CULL_RADIUS as f32;
            let start = origin + Vec3::new(0.0, -0.8 * r, 0.7 * r);
            self.shell
                .set_camera(Camera::new(start, origin, origin, gpu.aspect()));
        }
        self.editor.selected = None;
        self.sidebar.region = region;
        self.load_terrain(self.sidebar.seam_blend);
    }

    fn frame(&mut self) {
        if !self.shell.is_ready() || self.shell.camera().is_none() {
            return;
        }
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().min(0.1);
        self.last_frame = now;
        self.shell.camera_mut().unwrap().update(self.movement, dt);

        // `--live`: fold any server events that arrived since last frame into the world model,
        // so entity spawns and movement show up on the very next draw. Non-blocking — a slow
        // frame just batches more events. The player position is available for future follow-cam.
        if let Some(feed) = &self.live {
            let _ = live::drain_into(feed.events(), &mut self.world);
        }

        // Ask the world model for the near slice around the camera (grid-culled, parallel).
        let (cull_center, view_proj, cam_world) = {
            let c = self.shell.camera().unwrap();
            (
                c.cull_center(),
                c.view_proj().to_cols_array_2d(),
                c.world_pos,
            )
        };
        let count = {
            // `gpu` is Some here (guarded at the top of frame()); render_world fills the box
            // instances + pushes entity meshes straight into the GPU buffers.
            let gpu = self.shell.gpu_mut().unwrap();
            render_world(
                &self.world,
                self.origin,
                cull_center,
                CULL_RADIUS,
                self.entity_models.as_mut(),
                gpu,
                &mut self.instances,
                &mut self.mesh_instances,
                None,
                None,
                None,
                None,
                0.0,
                caer_render::anim_clock(),
            )
        };

        // Once-a-second FPS/ms, reused between ticks so the sidebar reads steady.
        self.frames += 1;
        if now - self.fps_timer >= std::time::Duration::from_secs(1) {
            self.sidebar.fps = self.frames;
            self.sidebar.ms_per_frame = 1000.0 / self.frames as f32;
            self.frames = 0;
            self.fps_timer = now;
        }

        // Fill the sidebar's read-model for this frame.
        self.sidebar.zone =
            match caer_world::zone_at(self.region, cam_world.x as i32, cam_world.y as i32) {
                Some(id) => {
                    caer_world::zone_name(id).map_or_else(|| format!("zone {id}"), str::to_string)
                }
                None => "— (between zones)".to_string(),
            };
        self.sidebar.visible = count;
        self.sidebar.cam_world = cam_world.to_array();
        self.sidebar.edit_mode = self.editor.on;
        self.sidebar.brush_patches = self
            .flatten_patches
            .iter()
            .filter(|f| f.region == self.region)
            .count();
        self.sidebar.selection = self.editor.selected.map(|i| {
            let e = &self.editor.instances[i];
            ui::SelectionInfo {
                zone_id: e.zone_id,
                fixture_id: e.fixture_id,
                yaw_deg: e.yaw.to_degrees().rem_euclid(360.0),
                pos: e.pos,
            }
        });

        // Build + tessellate the sidebar (needs the window; clone the Arc to avoid a self borrow).
        let win = self.shell.window().cloned().unwrap();
        let sidebar = &mut self.sidebar;
        let egui_frame = self
            .ui
            .as_mut()
            .map(|ui| ui.run(&win, |u| ui::build_sidebar(u, sidebar)));

        // Debug harness: `$CAER_TEST_SWAP=<region>` simulates picking that region in the Map
        // combo after ~2s of frames, then exits after a few more seconds — lets the windowed
        // swap path run under a headless-driven test (no way to click a combo over Wayland).
        self.frames_total += 1;
        if let Ok(list) = std::env::var("CAER_TEST_SWAP") {
            // Comma-separated region tour: swap every 300 frames, exit clean 300 after the last.
            let regions: Vec<u16> = list
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            if self.frames_total % 300 == 120 {
                let step = (self.frames_total / 300) as usize;
                if let Some(&region) = regions.get(step) {
                    println!("caer-render: TEST swap #{step} → region {region}");
                    std::io::stdout().flush().ok();
                    self.sidebar.region = region;
                } else if self.pending_load.is_none() {
                    // Only exit once the last load has actually arrived and applied.
                    println!(
                        "caer-render: TEST tour of {} swaps survived — exiting clean",
                        regions.len()
                    );
                    std::process::exit(0);
                }
            }
        }

        // Map combo picked a different region: reload the whole map.
        if self.sidebar.region != self.region {
            self.switch_region(self.sidebar.region);
        }

        // Terrain brush cursor rings track the mouse while a brush mode is active; a held
        // left button keeps stamping as the cursor sweeps (drag painting).
        let t_ring = Instant::now();
        self.update_brush_ring();
        let ring_ms = t_ring.elapsed().as_secs_f32() * 1000.0;
        let t_drag = Instant::now();
        self.brush_drag_tick();
        let drag_ms = t_drag.elapsed().as_secs_f32() * 1000.0;
        // Slow-frame instrumentation: name the culprit instead of guessing at the stutter.
        // Stamps log themselves (drag_ms covers them), so flag only a slow RING pass — it runs
        // every frame the brush is up and must stay well under a frame.
        if ring_ms > 8.0 {
            println!("caer-render: slow frame - ring {ring_ms:.1} ms | drag {drag_ms:.1} ms");
        }

        // Unsaved brush patches: write the store once the brush has been idle a moment.
        self.flush_flatten(false);

        // Terrain brush "Undo last patch".
        if self.sidebar.brush_undo {
            self.sidebar.brush_undo = false;
            self.brush_undo();
        }

        // Seam-blend slider "Apply": reload terrain at the new reach.
        if self.sidebar.apply_seam {
            self.sidebar.apply_seam = false;
            let b = self.sidebar.seam_blend_pending;
            self.sidebar.seam_blend = b;
            self.load_terrain(b);
        }

        // A background terrain load finished: apply it (unless a newer load superseded it).
        if let Some(rx) = &self.pending_load {
            match rx.try_recv() {
                Ok((gen, blend, mesh)) => {
                    self.pending_load = None;
                    if gen == self.load_gen {
                        self.finish_load(blend, mesh);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.pending_load = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }

        let gpu = self.shell.gpu_mut().unwrap();
        gpu.set_view_proj(view_proj);
        gpu.upload_instances(&self.instances);
        if gpu.render(count, egui_frame).is_err() {
            // Surface lost/outdated (e.g. resize or minimise): reconfigure to the window size.
            let size = win.inner_size();
            gpu.resize(size.width, size.height);
        } else if !self.logged_first {
            self.logged_first = true;
            println!("caer-render: renderer live — first frame drawn, {count} boxes visible");
            std::io::stdout().flush().ok();
        }
        if self.pending_load.is_some() {
            win.set_title(&format!(
                "caer-render — {} · loading terrain…",
                self.sidebar.zone
            ));
        } else {
            win.set_title(&format!("caer-render — {}", self.sidebar.zone));
        }
    }

    /// Toggle edit mode. Entering suspends the FPS title for the editor readout; leaving clears
    /// the selection.
    fn toggle_edit(&mut self) {
        self.editor.on = !self.editor.on;
        if !self.editor.on {
            self.editor.selected = None;
            self.sync_selection_box();
        }
        self.set_edit_title();
        println!(
            "caer-render: rotation editor {}",
            if self.editor.on { "ON" } else { "OFF" }
        );
    }

    /// Refresh the window title to reflect edit mode + current selection.
    fn set_edit_title(&self) {
        let Some(window) = self.shell.window() else {
            return;
        };
        if !self.editor.on {
            window.set_title("caer-render — fly mode");
            return;
        }
        let title = match self.editor.selected.map(|i| &self.editor.instances[i]) {
            Some(e) => format!(
                "caer-render ▸ EDIT zone{} fix{} · yaw {:.0}° · Q/E ±15 (Alt ±5) ,/. ±1 · WASD move · Space/Z up/down · R revert · auto-saves (Ctrl+S) · Tab exit",
                e.zone_id,
                e.fixture_id,
                e.yaw.to_degrees().rem_euclid(360.0),
            ),
            None => "caer-render ▸ EDIT · click a model to select · Tab exit".to_string(),
        };
        window.set_title(&title);
    }

    /// Cast a ray through the cursor and select the model whose *geometry* it actually hits
    /// nearest. Bounding spheres are only a broad phase (a big elm's sphere swallows the house
    /// beside it); every sphere candidate then gets an exact ray-vs-triangle test in model
    /// space, and the closest real surface hit wins. Selecting clears held movement so a key
    /// held while flying doesn't keep driving the camera once the same keys start transforming
    /// the picked asset.
    fn pick(&mut self) {
        let (Some(cam), Some(window)) = (self.shell.camera(), self.shell.window()) else {
            return;
        };
        let size = window.inner_size();
        let (cx, cy) = self.editor.cursor;
        let (ro, rd) = cam.screen_ray(cx, cy, size.width.max(1) as f32, size.height.max(1) as f32);
        let mut best: Option<(f32, usize)> = None;
        for (i, e) in self.editor.instances.iter().enumerate() {
            // Broad phase: bounding sphere.
            let center = e.center();
            let t = (center - ro).dot(rd);
            if t < 0.0 {
                continue; // sphere is behind the camera
            }
            let miss2 = (ro + rd * t - center).length_squared();
            if miss2 > e.radius * e.radius {
                continue;
            }
            let Some(pm) = self.editor.pick_meshes.get(e.batch) else {
                continue;
            };
            // Narrow phase: transform the ray into model space (undo translate, yaw, uniform
            // scale) and test the batch triangles. Model-space t × scale = world-space t.
            let (s, c) = e.yaw.sin_cos();
            let rel = ro - Vec3::from(e.pos);
            // Inverse yaw = rotate by -yaw about +Z.
            let mo = Vec3::new(c * rel.x + s * rel.y, -s * rel.x + c * rel.y, rel.z) / e.scale;
            let md = Vec3::new(c * rd.x + s * rd.y, -s * rd.x + c * rd.y, rd.z);
            for tri in pm.indices.chunks_exact(3) {
                let a = Vec3::from(pm.positions[tri[0] as usize]);
                let b = Vec3::from(pm.positions[tri[1] as usize]);
                let cc = Vec3::from(pm.positions[tri[2] as usize]);
                if let Some(tm) = ray_triangle(mo, md, a, b, cc) {
                    let tw = tm * e.scale;
                    if best.is_none_or(|(bt, _)| tw < bt) {
                        best = Some((tw, i));
                    }
                }
            }
        }
        self.editor.selected = best.map(|(_, i)| i);
        self.movement = Movement::default();
        self.set_edit_title();
        self.sync_selection_box();
    }

    /// Push the selected fixture's oriented bounding box to the GPU as the wireframe selection
    /// outline (or clear it when nothing is selected). Called on every selection or transform
    /// change so the box tracks the model live.
    fn sync_selection_box(&mut self) {
        let Some(gpu) = self.shell.gpu_mut() else {
            return;
        };
        let corners = self.editor.selected.map(|si| {
            let e = &self.editor.instances[si];
            let pm = &self.editor.pick_meshes[e.batch];
            let (s, c) = e.yaw.sin_cos();
            let mut out = [[0.0f32; 3]; 8];
            for (k, corner) in out.iter_mut().enumerate() {
                // bit 0 = +x, bit 1 = +y, bit 2 = +z (matches Gpu::set_selection_box edges)
                let l = [
                    if k & 1 != 0 { pm.max[0] } else { pm.min[0] },
                    if k & 2 != 0 { pm.max[1] } else { pm.min[1] },
                    if k & 4 != 0 { pm.max[2] } else { pm.min[2] },
                ];
                let (x, y) = (l[0] * e.scale, l[1] * e.scale);
                *corner = [
                    c * x - s * y + e.pos[0],
                    s * x + c * y + e.pos[1],
                    l[2] * e.scale + e.pos[2],
                ];
            }
            out
        });
        gpu.set_selection_box(corners);
    }

    /// Push the selected instance's current transform to the GPU, record it as an override, and
    /// persist. The override stores the effective yaw plus the move delta from the CSV position.
    fn apply_selected(&mut self) {
        let Some(si) = self.editor.selected else {
            return;
        };
        let e = &self.editor.instances[si];
        let dpos = [
            e.pos[0] - e.base_pos[0],
            e.pos[1] - e.base_pos[1],
            e.pos[2] - e.base_pos[2],
        ];
        let mi = terrain::ModelInstance {
            pos: e.pos,
            base_pos: e.base_pos,
            yaw: e.yaw,
            base_yaw: e.base_yaw,
            // The editor rotates about Z only, so an applied edit is a pure yaw by construction.
            rot: terrain::quat_axis_angle([0.0, 0.0, 1.0], e.yaw),
            base_rot: e.base_rot,
            scale: e.scale,
            zone_id: e.zone_id,
            fixture_id: e.fixture_id,
        };
        let (batch, inst, key) = (e.batch, e.inst, (e.zone_id, e.fixture_id));
        self.editor
            .overrides
            .insert(key, terrain::FixtureOverride { yaw: mi.yaw, dpos });
        if let Some(gpu) = self.shell.gpu() {
            gpu.update_model_instance(batch, inst, &mi);
        }
        self.persist_overrides();
        self.set_edit_title();
        self.sync_selection_box();
    }

    /// Write the override map to disk and report the outcome. Called after every edit (auto-save)
    /// and by the manual-save key. A failed write is surfaced loudly — silently losing hand edits
    /// is exactly the failure mode we already got bitten by once.
    fn persist_overrides(&self) {
        match terrain::save_fixture_overrides(&self.editor.overrides) {
            Ok(()) => println!(
                "caer-render: ✓ saved {} fixture override(s) → {}",
                self.editor.overrides.len(),
                terrain::fixture_overrides_path().display()
            ),
            Err(e) => eprintln!(
                "caer-render: ✗ FAILED to save fixture overrides to {}: {e}",
                terrain::fixture_overrides_path().display()
            ),
        }
    }

    /// Rotate the selected instance by `delta` radians (then apply + persist).
    fn rotate_selected(&mut self, delta: f32) {
        let Some(si) = self.editor.selected else {
            return;
        };
        self.editor.instances[si].yaw =
            (self.editor.instances[si].yaw + delta).rem_euclid(std::f32::consts::TAU);
        self.apply_selected();
    }

    /// Translate the selected instance by a world-space delta (units) — WASD move + Space/Z raise.
    fn nudge_selected(&mut self, delta: Vec3) {
        let Some(si) = self.editor.selected else {
            return;
        };
        let e = &mut self.editor.instances[si];
        e.pos[0] += delta.x;
        e.pos[1] += delta.y;
        e.pos[2] += delta.z;
        self.apply_selected();
    }

    /// Drop the selected instance's override, restoring its CSV-derived heading and position.
    fn revert_selected(&mut self) {
        let Some(si) = self.editor.selected else {
            return;
        };
        let (batch, inst, key, mi) = {
            let e = &mut self.editor.instances[si];
            e.yaw = e.base_yaw;
            e.pos = e.base_pos;
            (
                e.batch,
                e.inst,
                (e.zone_id, e.fixture_id),
                terrain::ModelInstance {
                    pos: e.base_pos,
                    base_pos: e.base_pos,
                    yaw: e.base_yaw,
                    base_yaw: e.base_yaw,
                    rot: e.base_rot,
                    base_rot: e.base_rot,
                    scale: e.scale,
                    zone_id: e.zone_id,
                    fixture_id: e.fixture_id,
                },
            )
        };
        self.editor.overrides.remove(&key);
        if let Some(gpu) = self.shell.gpu() {
            gpu.update_model_instance(batch, inst, &mi);
        }
        self.persist_overrides();
        self.set_edit_title();
        self.sync_selection_box();
    }

    /// True while a model is selected in edit mode — WASD/Space/Z then transform the asset instead
    /// of flying the camera.
    fn transforming(&self) -> bool {
        self.editor.on && self.editor.selected.is_some()
    }

    // ------------------------------ terrain brush ------------------------------

    /// Effective terrain height at world `(wx, wy)` from the WORKING heightfield (patches are
    /// baked in as strokes land, so this is a pure lattice lookup — no per-patch work).
    fn sample_height(&self, wx: f32, wy: f32) -> Option<f32> {
        let contains = |ox: i32, oy: i32| {
            let (zx0, zy0) = ((ox * ZONE_UNIT) as f32, (oy * ZONE_UNIT) as f32);
            (wx >= zx0 && wx < zx0 + 65_536.0 && wy >= zy0 && wy < zy0 + 65_536.0)
                .then_some((zx0, zy0))
        };
        let sample = |t: &caer_assets::terrain::ZoneTerrain, zx0: f32, zy0: f32| {
            t.height(((wx - zx0) / 256.0) as usize, ((wy - zy0) / 256.0) as usize)
        };
        // Consecutive samples almost always land in the same zone — try the last hit first.
        if let Some((ox, oy)) = self.last_sample_zone.get() {
            if let (Some((zx0, zy0)), Some(t)) =
                (contains(ox, oy), self.terrain_heights.get(&(ox, oy)))
            {
                return Some(sample(t, zx0, zy0));
            }
        }
        for (&(ox, oy), t) in &self.terrain_heights {
            if let Some((zx0, zy0)) = contains(ox, oy) {
                self.last_sample_zone.set(Some((ox, oy)));
                return Some(sample(t, zx0, zy0));
            }
        }
        None
    }

    /// Flush unsaved brush patches to the TSV store if the brush has been idle long enough
    /// (`force` = exit/reload paths, which must not lose edits). Debounced because a store
    /// write per stamp — on the fuseblk mount — was a large slice of the sculpting stutter.
    fn flush_flatten(&mut self, force: bool) {
        if !self.flatten_dirty || (!force && self.flatten_edited_at.elapsed().as_millis() < 1500) {
            return;
        }
        match terrain::save_flatten_regions(&self.flatten_patches) {
            Ok(()) => {
                self.flatten_dirty = false;
                println!(
                    "caer-render: brush patches saved ({} total)",
                    self.flatten_patches.len()
                );
            }
            Err(e) => eprintln!("caer-render: FAILED to save brush patches: {e}"),
        }
    }

    /// Cast the cursor ray and march it to the terrain surface. Returns the world-space hit.
    /// The camera ray is RENDER-space (origin-relative, y mirrored) — convert before sampling.
    fn terrain_hit(&self) -> Option<[f32; 3]> {
        let (cam, window) = (self.shell.camera()?, self.shell.window()?);
        let size = window.inner_size();
        let (cx, cy) = self.editor.cursor;
        let (ro, rd) = cam.screen_ray(cx, cy, size.width.max(1) as f32, size.height.max(1) as f32);
        // render → world: x adds origin, y mirrors around origin, z adds origin.
        let wo = Vec3::new(
            ro.x + self.origin.x,
            self.origin.y - ro.y,
            ro.z + self.origin.z,
        );
        let wd = Vec3::new(rd.x, -rd.y, rd.z).normalize();
        // Coarse march until we cross below ground, then bisect the crossing bracket.
        const STEP: f32 = 100.0;
        const MAX_T: f32 = 120_000.0;
        let mut t = 0.0;
        let mut last_above: Option<f32> = None;
        while t < MAX_T {
            let p = wo + wd * t;
            if let Some(h) = self.sample_height(p.x, p.y) {
                if p.z <= h {
                    let mut lo = last_above.unwrap_or(t);
                    let mut hi = t;
                    for _ in 0..24 {
                        let mid = (lo + hi) * 0.5;
                        let q = wo + wd * mid;
                        let under = self.sample_height(q.x, q.y).is_some_and(|hh| q.z <= hh);
                        if under {
                            hi = mid;
                        } else {
                            lo = mid;
                        }
                    }
                    let p = wo + wd * hi;
                    return Some([p.x, p.y, self.sample_height(p.x, p.y).unwrap_or(p.z)]);
                }
                last_above = Some(t);
            }
            t += STEP;
        }
        None
    }

    /// Apply one brush stroke at the cursor: author a flatten patch (target = clicked height,
    /// or the surrounding-ring mean in Smooth mode), persist it, bake it into the working
    /// heightfield, and re-mesh just the touched zones. Erase mode instead removes every patch
    /// centred under the brush and rebuilds that area.
    fn brush_apply(&mut self) {
        let t_start = Instant::now();
        let Some([hx, hy, hz]) = self.terrain_hit() else {
            println!("caer-render: brush - no terrain under cursor");
            return;
        };
        let hit_ms = t_start.elapsed().as_secs_f32() * 1000.0;
        let radius = self.sidebar.brush_radius;
        // Falloff is a fraction of the radius (like UE5), so small brushes are genuinely small.
        let falloff = (radius * self.sidebar.brush_falloff / 100.0).max(32.0);
        if self.sidebar.brush_mode == ui::BrushMode::Erase {
            return self.brush_erase(hx, hy, radius + falloff);
        }
        let target = match self.sidebar.brush_mode {
            // Level to the elevation where the stroke BEGAN (captured on the first stamp, held
            // for the drag). Chasing the cursor's current height instead raised terrain toward
            // every hump the brush passed over — the "flatten that un-flattens" bug.
            ui::BrushMode::Flatten => *self.brush_stroke_target.get_or_insert(hz),
            ui::BrushMode::Smooth => {
                // Mean ground height on the ring just outside the patch core - "what the
                // terrain around the hump agrees on". 16 samples is plenty at these scales.
                // Smooth stays LOCAL per stamp (it blends into current surroundings); only
                // Flatten holds a fixed reference.
                let ring = radius + falloff * 0.5;
                let mut sum = 0.0;
                let mut n = 0u32;
                for k in 0..16 {
                    let a = k as f32 * (std::f32::consts::TAU / 16.0);
                    if let Some(h) = self.sample_height(hx + ring * a.cos(), hy + ring * a.sin()) {
                        sum += h;
                        n += 1;
                    }
                }
                if n == 0 {
                    hz
                } else {
                    sum / n as f32
                }
            }
            _ => return,
        };
        let patch = terrain::FlattenRegion {
            region: self.region,
            cx: hx,
            cy: hy,
            radius,
            target_z: target,
            falloff,
        };
        // Persist in memory only; the TSV store catches up on the debounced flush.
        self.flatten_patches.push(patch);
        self.flatten_dirty = true;
        self.flatten_edited_at = Instant::now();
        // Bake into the working field incrementally (no replay of the whole log) and re-mesh
        // only the touched zones.
        let t_bake = Instant::now();
        terrain::apply_patch_to_field(&mut self.terrain_heights, &patch);
        let bake_ms = t_bake.elapsed().as_secs_f32() * 1000.0;
        self.remesh(Some(patch_rect(&patch)));
        println!(
            "caer-render: stamp - hit {hit_ms:.1} ms | bake {bake_ms:.1} ms | total {:.1} ms",
            t_start.elapsed().as_secs_f32() * 1000.0
        );
    }

    /// Erase mode: drop every patch centred within `reach` of the click, rebuild the working
    /// field over their combined footprint from pristine + survivors, re-mesh that area.
    fn brush_erase(&mut self, hx: f32, hy: f32, reach: f32) {
        // Purely in-memory: split the list into removed (this region, centre under the brush)
        // and kept; the TSV store catches up on the debounced flush.
        let region = self.region;
        let mut removed = Vec::new();
        self.flatten_patches.retain(|p| {
            let hit =
                p.region == region && ((p.cx - hx).powi(2) + (p.cy - hy).powi(2)).sqrt() <= reach;
            if hit {
                removed.push(*p);
            }
            !hit
        });
        if removed.is_empty() {
            println!("caer-render: erase - no patches under brush");
            return;
        }
        println!("caer-render: erased {} patch(es)", removed.len());
        self.flatten_dirty = true;
        self.flatten_edited_at = Instant::now();
        let mut rect = patch_rect(&removed[0]);
        for p in &removed[1..] {
            let r = patch_rect(p);
            rect = [
                rect[0].min(r[0]),
                rect[1].min(r[1]),
                rect[2].max(r[2]),
                rect[3].max(r[3]),
            ];
        }
        self.rebuild_and_remesh(rect);
    }

    /// Undo the most recent brush patch (any region - it's the list's last row) and rebuild
    /// exactly the area it covered.
    fn brush_undo(&mut self) {
        match self.flatten_patches.pop() {
            Some(p) => {
                println!("caer-render: undid last brush patch");
                self.flatten_dirty = true;
                self.flatten_edited_at = Instant::now();
                self.rebuild_and_remesh(patch_rect(&p));
            }
            None => println!("caer-render: brush - nothing to undo"),
        }
    }

    /// Recompute the working heightfield inside `rect` (pristine + surviving region patches,
    /// file order) and re-mesh the zones it touches - the undo/erase path.
    fn rebuild_and_remesh(&mut self, rect: [f32; 4]) {
        let survivors: Vec<terrain::FlattenRegion> = self
            .flatten_patches
            .iter()
            .filter(|f| f.region == self.region)
            .copied()
            .collect();
        terrain::rebuild_field_rect(
            &mut self.terrain_heights,
            &self.terrain_raw,
            &survivors,
            rect,
        );
        self.remesh(Some(rect));
    }

    /// Fast terrain-only rebuild after a brush edit: re-mesh the zones under `dirty` (the
    /// stroke's world-space footprint; None = all zones) from the retained heightfields and
    /// swap exactly those vertex buffers on the GPU — models/water/textures untouched.
    /// Restricting to the touched zones is what makes this interactive: a whole region is
    /// seconds of meshing, one zone is tens of milliseconds. NB: `on_ground` fixtures keep
    /// their old snap height until the next full reload ("Apply (reload terrain)").
    fn remesh(&mut self, dirty: Option<[f32; 4]>) {
        let t0 = Instant::now();
        let zones = terrain::remesh_terrain(
            &self.terrain_heights,
            &self.zone_order,
            self.origin,
            self.sidebar.seam_blend,
            dirty,
            self.color_scale,
        );
        let mesh_ms = t0.elapsed().as_secs_f32() * 1000.0;
        let n = zones.len();
        let t1 = Instant::now();
        if let Some(gpu) = self.shell.gpu_mut() {
            gpu.update_terrain_vertices(&zones);
        }
        println!(
            "caer-render: {n} zone(s) re-meshed - mesh {mesh_ms:.1} ms | upload {:.1} ms",
            t1.elapsed().as_secs_f32() * 1000.0
        );
    }

    /// Per-frame brush cursor: radius + falloff rings draped on the terrain under the mouse,
    /// plus a local sample-lattice grid so the heightfield's actual resolution is visible
    /// while sculpting. Cleared whenever the brush is off.
    fn update_brush_ring(&mut self) {
        let active = self.editor.on && self.sidebar.brush_mode != ui::BrushMode::Off;
        let hit = if active { self.terrain_hit() } else { None };
        type BrushRingLine = (Vec<[f32; 3]>, [f32; 3]);
        let lines: Option<Vec<BrushRingLine>> = hit.map(|[hx, hy, _]| {
            let origin = self.origin;
            let drape = |wx: f32, wy: f32| -> [f32; 3] {
                let z = self.sample_height(wx, wy).unwrap_or(0.0) + 15.0;
                [wx - origin.x, -(wy - origin.y), z - origin.z]
            };
            let ring = |r: f32| -> Vec<[f32; 3]> {
                let mut pts: Vec<[f32; 3]> = (0..48)
                    .map(|k| {
                        let a = k as f32 * (std::f32::consts::TAU / 48.0);
                        drape(hx + r * a.cos(), hy + r * a.sin())
                    })
                    .collect();
                pts.push(pts[0]); // close the loop
                pts
            };
            let radius = self.sidebar.brush_radius;
            let reach = radius * (1.0 + self.sidebar.brush_falloff / 100.0);
            let mut out = vec![
                (ring(radius), [0.35, 0.95, 0.4]),
                (ring(reach), [0.25, 0.55, 0.28]),
            ];
            // Local lattice grid: one polyline per 256-unit sample row/column crossing the
            // brush footprint (few hundred segments - trivial now sampling is a lookup).
            let g = caer_assets::terrain::SAMPLE_UNITS;
            let pad = reach + g * 2.0;
            let (x0, x1) = (((hx - pad) / g).floor() * g, ((hx + pad) / g).ceil() * g);
            let (y0, y1) = (((hy - pad) / g).floor() * g, ((hy + pad) / g).ceil() * g);
            let grid_col = [0.65, 0.75, 0.55];
            let mut x = x0;
            while x <= x1 {
                let mut line = Vec::new();
                let mut y = y0;
                while y <= y1 {
                    line.push(drape(x, y));
                    y += g;
                }
                out.push((line, grid_col));
                x += g;
            }
            let mut y = y0;
            while y <= y1 {
                let mut line = Vec::new();
                let mut x = x0;
                while x <= x1 {
                    line.push(drape(x, y));
                    x += g;
                }
                out.push((line, grid_col));
                y += g;
            }
            out
        });
        if let Some(gpu) = self.shell.gpu_mut() {
            gpu.set_brush_ring(lines.as_deref());
        }
    }

    /// Drag painting: while the left button is held with a brush mode active, re-stamp when
    /// the cursor has travelled far enough (and not too often), so a sweep reads as a stroke.
    fn brush_drag_tick(&mut self) {
        if !(self.brush_dragging && self.editor.on && self.sidebar.brush_mode != ui::BrushMode::Off)
        {
            return;
        }
        let Some([hx, hy, _]) = self.terrain_hit() else {
            return;
        };
        let min_dist = (self.sidebar.brush_radius * 0.45).max(48.0);
        let due = match self.last_stamp {
            None => true,
            Some((t0, [lx, ly])) => {
                t0.elapsed().as_millis() >= 90
                    && ((hx - lx).powi(2) + (hy - ly).powi(2)).sqrt() >= min_dist
            }
        };
        if due {
            self.last_stamp = Some((Instant::now(), [hx, hy]));
            self.brush_apply();
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.shell.is_ready() {
            return;
        }
        let window = match self
            .shell
            .init(event_loop, "caer-render — loading…", self.extent)
        {
            Ok(w) => w,
            Err(e) => {
                eprintln!("caer-render: GPU initialization failed:\n{e}");
                log::error!("GPU init failed: {e}");
                event_loop.exit();
                return;
            }
        };
        let gpu_aspect = self.shell.gpu().expect("gpu created").aspect();

        // Start above and to the south of the centroid, looking down over the field. Framing is
        // scaled to the cull radius so the opening shot fills with the visible slice.
        let r = CULL_RADIUS as f32;
        let start = self.origin + Vec3::new(0.0, -0.8 * r, 0.7 * r);
        let camera = Camera::new(start, self.origin, self.origin, gpu_aspect);
        let (min, max) = world_bbox(&self.world, self.region);
        self.world_min = min;
        self.world_max = max;
        self.editor.overrides = terrain::load_fixture_overrides();
        self.flatten_patches = terrain::load_flatten_regions();
        self.sidebar.brush_radius = 1500.0;
        self.sidebar.brush_falloff = 60.0; // percent of radius
        let blend = terrain::seam_blend();
        self.sidebar.seam_blend = blend;
        self.sidebar.seam_blend_pending = blend;
        self.sidebar.region = self.region;

        self.ui = Some(ui::Ui::new(&window));
        self.shell.set_camera(camera);
        self.load_terrain(blend);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Let the sidebar see the event first (it updates its own state regardless).
        let (egui_used, egui_kbd) = match (self.ui.as_mut(), self.shell.window()) {
            (Some(ui), Some(win)) => (ui.on_event(win, &event), ui.wants_keyboard()),
            _ => (false, false),
        };
        // Decide whether the app should skip this event. Pointer events defer to egui when it used
        // them (click/drag on the panel). Keyboard defers only while egui holds widget focus —
        // EXCEPT Tab, which is always the editor's (egui's combo keeps focus after use, and its
        // Tab-for-focus-nav would otherwise dead-lock the editor toggle until the app restarts).
        let skip = match &event {
            WindowEvent::KeyboardInput { event: k, .. } => {
                egui_kbd && k.physical_key != PhysicalKey::Code(KeyCode::Tab)
            }
            WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::CursorMoved { .. } => egui_used,
            _ => false,
        };
        // A click that lands in the 3D viewport (egui didn't use it) returns keyboard control:
        // drop egui's lingering widget focus so hotkeys work again.
        if !egui_used
            && matches!(
                &event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                }
            )
        {
            if let Some(ui) = self.ui.as_ref() {
                ui.drop_focus();
            }
        }
        match event {
            WindowEvent::CloseRequested => {
                self.flush_flatten(true);
                event_loop.exit();
            }
            _ if skip => {}
            WindowEvent::Resized(size) => self.shell.resize(size.width, size.height),
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                if let PhysicalKey::Code(code) = event.physical_key {
                    // Coarse rotation step: Alt drops it from 15° to a fine 1° (5° wasn't fine
                    // enough to line up long walls — Svarhamr); ,/. also step 1°.
                    let rot_step = if self.editor.alt { 1f32 } else { 15f32 }.to_radians();
                    // While a fixture is selected in edit mode, WASD/Space/Z transform it (discrete
                    // 5-unit / stepped nudges) instead of flying the camera. Movement keys fall
                    // through to the camera otherwise. World axes ("keeping north"): W +Y, D +X.
                    let xf = self.transforming();
                    match code {
                        KeyCode::Escape if pressed => {
                            self.flush_flatten(true);
                            event_loop.exit();
                        }
                        // Ctrl+S: manual save. Edits already auto-save (overrides immediately,
                        // brush patches on a debounce); this is a belt-and-braces flush.
                        KeyCode::KeyS if pressed && self.editor.ctrl => {
                            self.flush_flatten(true);
                            self.persist_overrides();
                        }
                        KeyCode::ShiftLeft | KeyCode::ShiftRight => self.movement.boost = pressed,
                        // --- fixture editor ---
                        KeyCode::Tab if pressed => self.toggle_edit(),
                        KeyCode::KeyQ if pressed && self.editor.on => {
                            self.rotate_selected(-rot_step)
                        }
                        KeyCode::KeyE if pressed && self.editor.on => {
                            self.rotate_selected(rot_step)
                        }
                        KeyCode::Comma if pressed && self.editor.on => {
                            self.rotate_selected(-1f32.to_radians())
                        }
                        KeyCode::Period if pressed && self.editor.on => {
                            self.rotate_selected(1f32.to_radians())
                        }
                        KeyCode::KeyR if pressed && self.editor.on => self.revert_selected(),
                        // --- move selected asset (edit + selection) vs fly camera ---
                        KeyCode::KeyW if pressed && xf => {
                            self.nudge_selected(Vec3::new(0.0, MOVE_STEP, screenshot_anim_time()))
                        }
                        KeyCode::KeyW => self.movement.forward = pressed,
                        KeyCode::KeyS if pressed && xf => {
                            self.nudge_selected(Vec3::new(0.0, -MOVE_STEP, screenshot_anim_time()))
                        }
                        KeyCode::KeyS => self.movement.back = pressed,
                        KeyCode::KeyA if pressed && xf => {
                            self.nudge_selected(Vec3::new(-MOVE_STEP, 0.0, screenshot_anim_time()))
                        }
                        KeyCode::KeyA => self.movement.left = pressed,
                        KeyCode::KeyD if pressed && xf => {
                            self.nudge_selected(Vec3::new(MOVE_STEP, 0.0, screenshot_anim_time()))
                        }
                        KeyCode::KeyD => self.movement.right = pressed,
                        KeyCode::Space if pressed && xf => {
                            self.nudge_selected(Vec3::new(0.0, 0.0, MOVE_STEP))
                        }
                        KeyCode::Space => self.movement.up = pressed,
                        KeyCode::KeyZ if pressed && xf => {
                            self.nudge_selected(Vec3::new(0.0, 0.0, -MOVE_STEP))
                        }
                        KeyCode::KeyC => self.movement.down = pressed,
                        _ => {}
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let notches = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 60.0,
                };
                if let Some(camera) = self.shell.camera_mut() {
                    camera.adjust_speed(notches);
                }
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.editor.alt = mods.state().alt_key();
                self.editor.ctrl = mods.state().control_key();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.editor.cursor = (position.x as f32, position.y as f32);
            }
            WindowEvent::MouseInput { button, state, .. } => {
                match button {
                    MouseButton::Right => self.looking = state == ElementState::Pressed,
                    // In edit mode the left button picks the model under the cursor instead of
                    // panning (or, with a brush mode active, stamps a terrain patch).
                    MouseButton::Left if self.editor.on => {
                        if state == ElementState::Pressed {
                            if self.sidebar.brush_mode != ui::BrushMode::Off {
                                self.brush_dragging = true;
                                self.last_stamp = None;
                                // New stroke: forget the last stroke's held elevation so the
                                // first stamp captures this press-point as the flatten target.
                                self.brush_stroke_target = None;
                                self.brush_drag_tick();
                            } else {
                                self.pick();
                            }
                        } else {
                            self.brush_dragging = false;
                            self.brush_stroke_target = None;
                        }
                    }
                    MouseButton::Left => self.panning = state == ElementState::Pressed,
                    _ => return,
                }
                if let Some(window) = self.shell.window() {
                    // Grab + hide the cursor while dragging so it doesn't leave the window.
                    let dragging = self.looking || self.panning;
                    let grab = if dragging {
                        CursorGrabMode::Confined
                    } else {
                        CursorGrabMode::None
                    };
                    let _ = window
                        .set_cursor_grab(grab)
                        .or_else(|_| window.set_cursor_grab(CursorGrabMode::Locked));
                    window.set_cursor_visible(!dragging);
                }
            }
            WindowEvent::RedrawRequested => self.frame(),
            _ => {}
        }
    }

    fn device_event(&mut self, _event_loop: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta: (dx, dy) } = event {
            // Don't spin the camera while the pointer is interacting with the sidebar.
            if self.ui.as_ref().is_some_and(ui::Ui::wants_pointer) {
                return;
            }
            if let Some(camera) = self.shell.camera_mut() {
                if self.looking {
                    camera.look(dx as f32, dy as f32);
                } else if self.panning {
                    camera.pan(dx as f32, dy as f32);
                }
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = self.shell.window() {
            window.request_redraw();
        }
    }
}
