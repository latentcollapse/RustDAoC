//! Runtime cost axes: where the time goes, how much memory the assets want, and whether an
//! authored layout overlaps itself.
//!
//! # Why these exist
//!
//! Matt reported a stutter entering character select and again entering create. Nothing measured
//! it, so every explanation was a guess. Nothing measured asset memory either, and the Options
//! dialog's arrow overlap was argued about from screenshots rather than from its own geometry.
//!
//! These are **measurements with thresholds only where a threshold is defensible**. Timing numbers
//! vary by machine and disk, so the timing test reports and only fails on an order-of-magnitude
//! regression; the layout test has an exact geometric answer and asserts it.
//!
//! **Fails** when the retail tree is absent (REQ-025). Set `CAER_CLIENT`.

mod common;

use common::root;

use std::time::Instant;

use caer_render::entities::EntityModels;
use caer_render::gpu::Gpu;

/// **D1 — where the first-use stutter actually goes.**
///
/// Every stage the character screens perform on first entry, timed cold (first call, nothing
/// cached) and warm (repeat). The stutter Matt reported is the cold column; the warm column is
/// what a second visit costs. A stage whose cold time dwarfs the others is the one to move off the
/// render path, and until this ran nobody knew which stage that was.
///
/// Reports rather than gates: absolute milliseconds depend on the disk (this tree lives on an
/// NTFS-over-FUSE spinning disk, ~33x slower than NVMe). The assertion is only that warm is not
/// catastrophically worse than cold, which would mean the caches are actively harmful.
#[test]
fn preworld_stage_timings_cold_and_warm() {
    let root = root();
    // SAFETY: single-threaded test entry; loaders read the client root from env.
    std::env::set_var("CAER_CLIENT", &root);
    let mut gpu = pollster::block_on(Gpu::new_headless(320, 240, 60_000.0)).expect("gpu init");

    let t = Instant::now();
    let mut models = EntityModels::load().expect("entity models");
    let entity_tables = t.elapsed();

    // The PRODUCT path is `load_cached`; `load` is the raw decode behind it. Timing the raw decode
    // twice measures the disk, not what a player pays on a second visit.
    let t = Instant::now();
    let scene_cold = caer_render::preworld_scene::load_cached(&root, 1);
    let scene_cold_ms = t.elapsed();
    let t = Instant::now();
    let _scene_warm = caer_render::preworld_scene::load_cached(&root, 1);
    let scene_warm_ms = t.elapsed();

    // Avatar assembly: cold is a full fig3 part decode + skin bind; warm hits the mesh cache.
    let t = Instant::now();
    let _ = models.ensure_avatar(
        &mut gpu,
        1,
        1,
        caer_protocol::customization::Customization::default(),
        None,
    );
    let avatar_cold = t.elapsed();
    let t = Instant::now();
    let _ = models.ensure_avatar(
        &mut gpu,
        1,
        1,
        caer_protocol::customization::Customization::default(),
        None,
    );
    let avatar_warm = t.elapsed();

    // A second race pays the cold cost again — this is what switching race on the create form does.
    let t = Instant::now();
    let _ = models.ensure_avatar(
        &mut gpu,
        6,
        1,
        caer_protocol::customization::Customization::default(),
        None,
    );
    let second_race_cold = t.elapsed();

    let scene = scene_cold.expect("Albion scene");
    let verts: usize = scene.models.iter().map(|m| m.vertices.len()).sum();

    println!("\n=== preworld stage timings ===");
    println!(
        "  entity tables (gamedata.mpk)   {:>9.1} ms",
        entity_tables.as_secs_f64() * 1000.0
    );
    println!(
        "  scene load  cold               {:>9.1} ms  ({verts} verts, {} textures)",
        scene_cold_ms.as_secs_f64() * 1000.0,
        scene.textures.len()
    );
    println!(
        "  scene load  warm               {:>9.1} ms",
        scene_warm_ms.as_secs_f64() * 1000.0
    );
    println!(
        "  avatar      cold (Briton)      {:>9.1} ms",
        avatar_cold.as_secs_f64() * 1000.0
    );
    println!(
        "  avatar      warm (Briton)      {:>9.1} ms",
        avatar_warm.as_secs_f64() * 1000.0
    );
    println!(
        "  avatar      cold (Troll, 2nd)  {:>9.1} ms",
        second_race_cold.as_secs_f64() * 1000.0
    );
    println!(
        "\n  the cold column is the stutter. Anything here above ~16 ms cannot happen on the\n  \
         render thread without dropping a frame."
    );

    // A cached scene must be effectively free. Before the cache both columns read ~810 ms, which
    // is the hitch entering and re-entering the character screens.
    assert!(
        scene_warm_ms < std::time::Duration::from_millis(50),
        "warm scene load took {:.1} ms — the scene cache is not working",
        scene_warm_ms.as_secs_f64() * 1000.0
    );
    assert!(
        avatar_warm <= avatar_cold * 4 + std::time::Duration::from_millis(5),
        "warm avatar assembly ({:.1} ms) is far worse than cold ({:.1} ms) — the cache is hurting",
        avatar_warm.as_secs_f64() * 1000.0,
        avatar_cold.as_secs_f64() * 1000.0
    );
}

/// **D3 — how much asset memory the character screens ask for.**
///
/// Vertex, index and texture bytes for one realm scene plus a full set of bodies. This is the
/// number a VRAM budget is written against, and nothing reported it before.
#[test]
fn preworld_asset_memory_footprint() {
    let root = root();
    // SAFETY: single-threaded test entry; loaders read the client root from env.
    std::env::set_var("CAER_CLIENT", &root);

    let mut total_tex = 0usize;
    let mut total_vert = 0usize;
    let mut total_idx = 0usize;

    println!("\n=== preworld asset footprint ===");
    println!(
        "  {:<10} {:>10} {:>12} {:>12} {:>12}",
        "realm", "verts", "vertex KiB", "index KiB", "texture KiB"
    );
    for (realm, name) in [(1u8, "Albion"), (2, "Midgard"), (3, "Hibernia")] {
        let Some(scene) = caer_render::preworld_scene::load(&root, realm) else {
            continue;
        };
        let verts: usize = scene.models.iter().map(|m| m.vertices.len()).sum();
        let idx: usize = scene.models.iter().map(|m| m.indices.len()).sum();
        let vbytes = verts * std::mem::size_of::<caer_render::terrain::TerrainVertex>();
        let ibytes = idx * std::mem::size_of::<u32>();
        let tbytes: usize = scene
            .textures
            .values()
            .map(|t| t.mips.iter().map(Vec::len).sum::<usize>())
            .sum();
        println!(
            "  {name:<10} {verts:>10} {:>12} {:>12} {:>12}",
            vbytes / 1024,
            ibytes / 1024,
            tbytes / 1024
        );
        total_vert += vbytes;
        total_idx += ibytes;
        total_tex += tbytes;
    }
    println!(
        "  {:<10} {:>10} {:>12} {:>12} {:>12}",
        "TOTAL",
        "",
        total_vert / 1024,
        total_idx / 1024,
        total_tex / 1024
    );
    println!(
        "\n  all three realm scenes: {:.1} MiB of decoded assets",
        (total_vert + total_idx + total_tex) as f64 / (1024.0 * 1024.0)
    );

    // A scene bank that decodes to hundreds of MiB would not fit alongside a world; this is a
    // sanity ceiling, deliberately loose, not a tuned budget.
    let mib = (total_vert + total_idx + total_tex) as f64 / (1024.0 * 1024.0);
    assert!(
        mib < 512.0,
        "the three character-screen scenes decode to {mib:.1} MiB — that is a leak or a decode bug"
    );
}

/// **C1 — the Options dialog does not overlap itself.**
///
/// There is no authored oracle for this dialog: it is drawn from `game.dll` and the client ships
/// no XML for it, so `OPTIONS_ROWS` is entirely our construction. That means no reference tells us
/// the right pitch — but self-consistency is still checkable, and self-overlap is a defect on any
/// reading. `ARROW_SIZE` is oracle-backed at 16 (`pregame/styles.xml`), so the row pitch has to
/// clear it.
///
/// Fails when a row's drawn control collides with the row above or below it, which is exactly the
/// arrow overlap visible on screen.
#[test]
fn options_rows_do_not_collide_vertically() {
    use caer_render::preworld_options as opt;

    let cycles: Vec<&opt::OptionsRow> = opt::OPTIONS_ROWS
        .iter()
        .filter(|r| r.kind == opt::RowKind::Cycle)
        .collect();
    assert!(cycles.len() > 4, "expected several cycle rows");

    println!(
        "\nARROW_SIZE {} / ROW_PITCH {} / ROW_H {}",
        opt::ARROW_SIZE,
        opt::ROW_PITCH,
        opt::ROW_H
    );

    let mut collisions: Vec<String> = Vec::new();
    for pair in cycles.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a.col != b.col || b.row != a.row + 1 {
            continue;
        }
        let [top, _] = opt::arrow_rects(a);
        let [below, _] = opt::arrow_rects(b);
        let overlap = (top.1 + top.3) - below.1;
        if overlap > 0.0 {
            collisions.push(format!("{} / {} overlap {overlap:.0}px", a.label, b.label));
        }
    }

    // REPORTED, NOT ASSERTED — and the reason matters.
    //
    // The overlap is real and measured: 8 pairs, 3px each, which is what Matt sees. But the fix is
    // not derivable from anything we hold. Raising ROW_PITCH to clear a 16px arrow pushes the last
    // of 30 authored rows to y=614 in a dialog that ends at 548, so the pitch cannot simply grow.
    // Retail fits all 30 rows in that space with `button_left` declared 16x16 in `styles.xml`,
    // which means either its drawn glyph carries transparent padding inside the cell, or its
    // dialog geometry differs from ours. Both are answerable — by measuring the arrow sprite's
    // opaque extent in the skin texture — and neither is answerable by picking a number here.
    //
    // Asserting zero overlap would force a guess into the layout; asserting the current value
    // would pin the defect. So this states the measurement and leaves the decision visible.
    let max_overlap = 3.0_f32;
    println!(
        "\n  {} adjacent cycle-row pairs overlap: {}",
        collisions.len(),
        if collisions.is_empty() {
            "none".to_string()
        } else {
            collisions.join(", ")
        }
    );
    println!(
        "  OPEN: ARROW_SIZE {} is oracle-backed (styles.xml); ROW_PITCH {} is ours and has no\n  \
         oracle. Resolve by measuring the arrow sprite's opaque extent, not by choosing a pitch.",
        opt::ARROW_SIZE,
        opt::ROW_PITCH
    );

    // What IS assertable: the overlap must not grow. A regression past the known 3px means someone
    // changed a layout constant without resolving the open question above.
    for c in &collisions {
        let px: f32 = c
            .rsplit(' ')
            .next()
            .and_then(|s| s.trim_end_matches("px").parse().ok())
            .unwrap_or(0.0);
        assert!(
            px <= max_overlap,
            "row overlap grew past the known {max_overlap}px: {c}"
        );
    }
}

/// **D2 — the window contract, as far as it can be checked without a compositor.**
///
/// `apply()` talks to a live compositor and cannot be unit-tested (see its doc comment). What CAN
/// be checked is that the pure intent layer is total and self-consistent across every setting the
/// UI can produce, so a compositor-specific bug is never confused with a logic bug in our own
/// translation. R-1 requires this hold on KDE, GNOME, COSMIC, wlroots and X11; this covers the
/// half that does not need a session.
///
/// Fails if any mode/flag combination produces an intent that contradicts what the UI shows: a
/// dynamic windowed request carrying a fixed size, a fullscreen mode carrying one, or a resolution
/// that survives being sanitised away.
#[test]
fn window_intent_is_total_and_self_consistent() {
    use caer_render::{DisplayMode, DisplaySettings, FullscreenIntent, WindowMode};

    let sizes = [
        DisplayMode {
            width: 1280,
            height: 720,
            refresh_millihertz: 0,
        },
        DisplayMode {
            width: 1920,
            height: 1080,
            refresh_millihertz: 60_000,
        },
        DisplayMode {
            width: 3840,
            height: 2160,
            refresh_millihertz: 144_000,
        },
    ];
    let mut checked = 0usize;
    println!(
        "\n{:<12} {:<8} {:>12} {:>22}",
        "mode", "dynamic", "size", "intent"
    );
    for mode in [
        WindowMode::Windowed,
        WindowMode::Borderless,
        WindowMode::Exclusive,
    ] {
        for dynamic in [false, true] {
            for size in sizes {
                let s = DisplaySettings {
                    mode,
                    use_current_window: dynamic,
                    mode_size: size,
                    ..DisplaySettings::default()
                };
                let i = s.window_intent();
                println!(
                    "{:<12?} {dynamic:<8} {:>5}x{:<6} {:>22}",
                    mode,
                    size.width,
                    size.height,
                    format!("{:?} {:?}", i.fullscreen, i.inner_size)
                );
                checked += 1;

                match mode {
                    // Windowed is the only mode that carries a size, and it always carries one.
                    WindowMode::Windowed => {
                        assert_eq!(i.fullscreen, FullscreenIntent::None, "{mode:?}");
                        if dynamic {
                            assert_eq!(
                                i.inner_size,
                                Some((
                                    caer_render::WINDOWED_FALLBACK.width,
                                    caer_render::WINDOWED_FALLBACK.height
                                )),
                                "windowed with no size of the player's own must land on a window, \
                                 not keep whatever it already had"
                            );
                        } else {
                            assert_eq!(
                                i.inner_size,
                                Some((size.width, size.height)),
                                "fixed windowed must request exactly the chosen size"
                            );
                        }
                    }
                    // Fullscreen modes take the monitor; carrying a window size would fight it.
                    WindowMode::Borderless => {
                        assert_eq!(i.fullscreen, FullscreenIntent::Borderless);
                        assert_eq!(i.inner_size, None);
                    }
                    WindowMode::Exclusive => {
                        assert_eq!(i.fullscreen, FullscreenIntent::Exclusive);
                        assert_eq!(i.inner_size, None);
                    }
                }
            }
        }
    }
    assert_eq!(
        checked, 18,
        "every mode x dynamic-flag x size combination must be covered"
    );
    println!(
        "\n  {checked} combinations, all self-consistent. NOT covered here, and NOT claimed:\n  \
         whether a compositor honours any of it. `request_inner_size` is advisory, exclusive\n  \
         fullscreen is an optional protocol, and leaving fullscreen can recreate the surface.\n  \
         Those need a live session per desktop — see ticket R-1."
    );
}

/// **C1 follow-up — how much of the arrow's 16x16 cell is actually opaque.**
///
/// `styles.xml` declares `button_left` at 16x16 and points it at the `slider` texture page. Our
/// row pitch is 13, so the declared cells of adjacent rows overlap by 3px — but a declared cell is
/// not a drawn glyph. If the sprite carries transparent padding, retail fits 30 rows at this pitch
/// with no visible collision and our layout needs no change at all; if it is opaque edge to edge,
/// the pitch or the dialog height has to give.
///
/// This measures the sprite's opaque extent so the decision is made from the art rather than from
/// a screenshot. Reports only — the fix belongs to whoever acts on the number.
#[test]
fn arrow_sprite_opaque_extent() {
    let root = root();
    // The page `styles.xml` names for this control.
    let mut found = false;
    // `pregame/asset.xml` resolves this page to `archive://pregame/pregame.mpk:slider.tga`, so it
    // is an archive member, not a loose file.
    for rel in ["slider.tga", "slider.dds"] {
        let Ok(Some(bytes)) = caer_assets::open_member(root.join("pregame/pregame.mpk"), rel)
        else {
            continue;
        };
        let img = if rel.ends_with(".tga") {
            caer_assets::tga::decode(&bytes)
                .ok()
                .map(|i| (i.width, i.height, i.rgba))
        } else {
            caer_assets::dds::read_model_dds(&bytes)
                .ok()
                .and_then(|t| t.rgba8_mip0())
        };
        let Some((w, h, rgba)) = img else { continue };
        found = true;
        println!("\n{rel}: {w}x{h}");

        // Per 16x16 cell across the page, how many rows/columns carry any opaque pixel.
        let cells_x = w / 16;
        let cells_y = h / 16;
        let mut reported = 0usize;
        for cy in 0..cells_y {
            for cx in 0..cells_x {
                let (mut min_y, mut max_y, mut opaque) = (16u32, 0u32, 0usize);
                for y in 0..16 {
                    for x in 0..16 {
                        let px = ((cy * 16 + y) * w + cx * 16 + x) as usize * 4;
                        if rgba.get(px + 3).is_some_and(|a| *a > 16) {
                            opaque += 1;
                            min_y = min_y.min(y);
                            max_y = max_y.max(y);
                        }
                    }
                }
                if opaque == 0 || reported >= 6 {
                    continue;
                }
                reported += 1;
                println!(
                    "  cell ({cx},{cy}): {opaque:>3} opaque px, rows {min_y}..={max_y} \
                     -> {} of 16 rows used",
                    max_y - min_y + 1
                );
            }
        }
        println!(
            "  a cell using <= 13 rows means the 16px declaration includes padding and our\n  \
             ROW_PITCH of 13 draws no visible collision; > 13 means the pitch must change."
        );
        break;
    }
    if !found {
        println!(
            "\nthe `slider` page named by styles.xml was not found under the paths tried — \
                  the arrow's opaque extent stays unmeasured"
        );
    }
}

/// **Where the character and the landmarks actually land on screen.**
///
/// Matt's report is that characters stand off to the side instead of front and centre. Reasoning
/// about the camera says the anchor must project to the middle, so the disagreement is exactly the
/// kind that needs a number: this projects the subject anchor and each landmark through the real
/// camera and prints normalised screen X, where 0.5 is centre.
///
/// Fails if the subject is not near centre — that is the composition the screens are built on.
#[test]
fn subject_projects_to_screen_centre() {
    let root = root();
    let (w, h) = (1920.0_f32, 1080.0_f32);
    let mut off: Vec<String> = Vec::new();

    for (realm, name) in [(1u8, "Albion"), (2, "Midgard"), (3, "Hibernia")] {
        let Some(scene) = caer_render::preworld_scene::load_cached(&root, realm) else {
            continue;
        };
        let (eye, focus, feet) =
            caer_render::preworld_scene::framing_around_character(&scene, 70.0, None, 16.0 / 9.0);
        // The same construction `aim_preworld_camera` performs, mirror included.
        let unmirror = |v: glam::Vec3| glam::Vec3::new(v.x, -v.y, v.z);
        let mut cam = caer_render::camera::Camera::new(
            unmirror(eye),
            unmirror(focus),
            glam::Vec3::ZERO,
            w / h,
        );
        cam.ensure_far(60_000.0);
        let vp = cam.view_proj();

        // Avatar world position, as `stand_preworld_avatar` places it.
        let avatar = glam::Vec3::new(feet.x, feet.y, feet.z + 35.0);
        let project = |p: glam::Vec3| -> Option<(f32, f32)> {
            let c = vp * glam::Vec4::new(p.x, p.y, p.z, 1.0);
            if c.w.abs() < 1e-6 {
                return None;
            }
            Some((0.5 + 0.5 * c.x / c.w, 0.5 - 0.5 * c.y / c.w))
        };
        let Some((ax, ay)) = project(avatar) else {
            continue;
        };
        // Project the FOCUS too. If the focus is not at 0.5 the camera is not looking where it was
        // told; if the focus IS centred but the avatar is not, the avatar is placed off-anchor.
        let fx = project(focus).map(|(x, _)| x);
        println!(
            "\n{name}: subject x={ax:.3} y={ay:.3}   focus x={}   (0.5 = centre)",
            fx.map_or("-".into(), |v| format!("{v:.3}"))
        );
        if (ax - 0.5).abs() > 0.08 {
            off.push(format!("{name} subject at x={ax:.3}"));
        }
    }

    assert!(
        off.is_empty(),
        "the subject must frame near centre; measured: {}",
        off.join(", ")
    );
}

/// **The pre-world coordinate contract.**
///
/// One canonical 1024×768 design space, one transform, one window→surface conversion, and hit
/// rects that are the visible button art rather than the caption beneath it or a box drawn round
/// both. The defect this replaces: on the create form, `hit_charcreate` tested the `64x16_no_bg`
/// word at y=752 while the round button was drawn at y=706, so Cancel, Continué and Realm were
/// inert exactly where the player could see them.
///
/// Every viewport here is a real case — the authored size, a 16:9 display that pillarboxes, a
/// 16:10 one that letterboxes top and bottom, and a window whose size disagrees with the surface
/// (the resize race, where the cursor arrives against the new window and the frame is still the
/// old swapchain).
#[test]
fn every_preworld_control_responds_where_its_art_is() {
    use caer_render::preworld::{PreWorldScreen, PreworldTransform};
    use caer_render::preworld_hitbox::{self, window_to_surface};

    // (surface, window). Equal except in the last case.
    let cases = [
        ((1024.0_f32, 768.0_f32), (1024.0_f32, 768.0_f32)),
        ((1920.0, 1080.0), (1920.0, 1080.0)),
        ((1280.0, 800.0), (1280.0, 800.0)),
        ((800.0, 600.0), (800.0, 600.0)),
        ((1920.0, 1080.0), (3840.0, 2160.0)),
    ];
    let screens = [
        PreWorldScreen::RealmSelect,
        PreWorldScreen::CharSelect,
        PreWorldScreen::CharCreate,
    ];

    let mut checked = 0usize;
    for (surface, window) in cases {
        let xf = PreworldTransform::from_viewport(surface);
        // Window pixels in, surface pixels out — the client's own conversion, used once.
        let press = |dx: f32, dy: f32| {
            let (sx, sy) = xf.map(dx, dy);
            let (wx, wy) = (sx * window.0 / surface.0, sy * window.1 / surface.1);
            let s = window_to_surface(wx, wy, window, surface);
            (s[0], s[1])
        };

        for screen in screens {
            let table = preworld_hitbox::controls(screen);
            // `character_creation.xml` really does place the Random button two pixels into the
            // name edit box, and the front-most control wins there. Skipping those two pixels is
            // not a tolerance: it is the client's own layering, and `preworld_hitbox`'s
            // `controls_do_not_overlap_within_a_screen` is what keeps the exception a short list.
            let shadowed = |c: &preworld_hitbox::Control, x: f32, y: f32| {
                table
                    .iter()
                    .take_while(|o| o.target != c.target)
                    .any(|o| o.covers(x, y))
            };
            for c in table {
                for part in &c.parts {
                    // Centre and all four inside edges take the control.
                    let inside = [
                        (part.x + part.w * 0.5, part.y + part.h * 0.5),
                        (part.x + 0.5, part.y + part.h * 0.5),
                        (part.x + part.w - 0.5, part.y + part.h * 0.5),
                        (part.x + part.w * 0.5, part.y + 0.5),
                        (part.x + part.w * 0.5, part.y + part.h - 0.5),
                    ];
                    for (dx, dy) in inside {
                        if shadowed(c, dx, dy) {
                            continue;
                        }
                        let (sx, sy) = press(dx, dy);
                        assert_eq!(
                            preworld_hitbox::hit(screen, sx, sy, surface).map(|h| h.target),
                            Some(c.target),
                            "{surface:?}/{window:?} {screen:?} {} id {}: ({dx},{dy}) is inside the art",
                            c.name,
                            part.control_id
                        );
                        checked += 1;
                    }
                }

                // One pixel outside every part, in each direction, hits nothing — unless it lands
                // on a neighbouring control, which is a legitimate answer and not this control.
                let b = c.bounds();
                for (dx, dy) in [
                    (b.x - 1.0, b.y + b.h * 0.5),
                    (b.x + b.w + 1.0, b.y + b.h * 0.5),
                    (b.x + b.w * 0.5, b.y - 1.0),
                    (b.x + b.w * 0.5, b.y + b.h + 1.0),
                ] {
                    if c.covers(dx, dy) {
                        continue;
                    }
                    let (sx, sy) = press(dx, dy);
                    assert_ne!(
                        preworld_hitbox::hit(screen, sx, sy, surface).map(|h| h.target),
                        Some(c.target),
                        "{surface:?}/{window:?} {screen:?} {}: ({dx},{dy}) is outside its art and still activates it",
                        c.name
                    );
                    checked += 1;
                }
            }

            // Letterbox bars belong to no control.
            for (sx, sy) in [
                (xf.ox * 0.5, surface.1 * 0.5),
                (surface.0 - xf.ox * 0.5, surface.1 * 0.5),
                (surface.0 * 0.5, xf.oy * 0.5),
                (surface.0 * 0.5, surface.1 - xf.oy * 0.5),
            ] {
                let p = preworld_hitbox::probe(screen, sx, sy, surface);
                if p.on_plate {
                    continue; // no bar on this axis at this aspect
                }
                assert!(
                    p.control.is_none(),
                    "{surface:?} {screen:?}: a point in the black bar selected {:?}",
                    p.control.map(|c| c.target)
                );
                checked += 1;
            }
        }
    }
    assert!(
        checked > 2000,
        "only {checked} probes ran — the control table or the case list shrank"
    );
}

/// Hover and click resolve to the same control, through the HUD the client actually runs.
///
/// The geometry test above is over pure functions. This one goes through `PreWorldHud`, because
/// the two used to disagree: the draw asked whether `EnterWorld` was hovered while the hit test
/// produced `OpenCharCreate`, so the Play button never lit unless a character was selected.
#[test]
fn hover_and_click_select_the_same_control() {
    use caer_render::preworld::{PreWorldAction, PreWorldHud, PreWorldScreen, PreworldTransform};
    use caer_render::preworld_hitbox::{self, Target};

    for surface in [(1024.0_f32, 768.0_f32), (1920.0, 1080.0), (1280.0, 800.0)] {
        let xf = PreworldTransform::from_viewport(surface);
        for screen in [
            PreWorldScreen::RealmSelect,
            PreWorldScreen::CharSelect,
            PreWorldScreen::CharCreate,
        ] {
            let mut hud = PreWorldHud::new(std::path::PathBuf::from("/nonexistent"));
            hud.set_screen(screen);
            for c in preworld_hitbox::controls(screen) {
                let (dx, dy) = c.click_point();
                let (sx, sy) = xf.map(dx, dy);
                let click = hud.hit_action(sx, sy, surface);
                hud.set_pointer(sx, sy, surface);
                assert_eq!(
                    hud.hover_state().action,
                    click,
                    "{surface:?} {screen:?} {}: hover and click disagree",
                    c.name
                );
                // Disabled controls route nothing by design (H1); everything else must act.
                // Delete left this list when B5 gave it a complete path — confirm form, dispatch and
                // wire packet. Customize is the last one, and stays until B3 implements
                // `character_customize.xml`.
                let disabled = matches!(c.target, Target::CharSelectCustomize);
                assert_eq!(
                    click.is_none(),
                    disabled,
                    "{surface:?} {screen:?} {}: expected disabled={disabled}, got {click:?}",
                    c.name
                );
            }

            // Moving onto empty plate clears the hover rather than latching the last control.
            //
            // This used to probe the letterbox bar, behind `if xf.ox > 1.0`. When the plate began
            // filling the surface (A11) that guard went permanently false and the check **silently
            // stopped running** — a passing assertion that asserts nothing, which is worse than a
            // deleted one because the suite still counts it. Re-aimed at a design-space point that
            // is on the plate and over no control, which is the property that actually mattered.
            let empty = xf.map(512.0, 690.0); // between the bottom-row controls
            assert!(
                preworld_hitbox::hit(screen, empty.0, empty.1, surface).is_none(),
                "{surface:?} {screen:?}: probe point must be over no control, or this proves nothing"
            );
            hud.set_pointer(empty.0, empty.1, surface);
            assert_eq!(
                hud.hover_state().action,
                None,
                "{surface:?} {screen:?}: empty plate left a control hovered"
            );
        }
        // Realm hover is the one highlight that is not an action, so check it separately.
        let mut hud = PreWorldHud::new(std::path::PathBuf::from("/nonexistent"));
        hud.set_screen(PreWorldScreen::RealmSelect);
        let alb = preworld_hitbox::control_for(PreWorldScreen::RealmSelect, Target::RealmColumn(1))
            .expect("albion");
        let (dx, dy) = alb.click_point();
        let (sx, sy) = xf.map(dx, dy);
        hud.set_pointer(sx, sy, surface);
        assert_eq!(
            hud.hit_action(sx, sy, surface),
            Some(PreWorldAction::ChooseRealm(1))
        );
        assert!(
            hud.hovered_realm().is_some(),
            "{surface:?}: crest not hovered"
        );
    }
}

/// The head landmark must be in rendered coordinates, which means it carries `SkinnedRig::z_offset`.
///
/// fig3 parts are authored with their origin above the feet, so `ensure_avatar` foot-anchors the
/// body with `z_offset = -bound_min.z` and every palette translation gets it before the instance
/// scale applies. A landmark that omits it aims below the rendered head by `z_offset * scale` —
/// visible in a capture as a target that sits too low on the body.
///
/// Contract only: this proves the formula, not that the runtime rig's offset reaches the camera.
#[test]
fn the_head_landmark_is_foot_anchored_like_the_rendered_body() {
    use caer_assets::figures::FigureModels;

    let root = root();
    let figs_root = root.join("figures/fig3");
    let mut archives: Vec<_> = std::fs::read_dir(&figs_root)
        .expect("fig3 dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().starts_with("sfig"))
        })
        .collect();
    archives.sort();
    let tables = caer_assets::anims::AnimTables::load(root.join("gamedata.mpk")).expect("anims");

    let want = FigureModels::skeleton_member(1, 1).expect("briton male skeleton");
    let skel = archives
        .iter()
        .find_map(|a| {
            let ms = caer_assets::open(a).ok()?;
            let m = ms.iter().find(|m| m.name.eq_ignore_ascii_case(&want))?;
            caer_assets::nif::read_skeleton(&m.data).ok()
        })
        .expect("briton male ships a skeleton");
    let cref = tables
        .set_for_race_gender("Briton", "Male")
        .and_then(|set| tables.clip(set, caer_assets::anims::Action::Idle))
        .expect("briton male idle");
    let path = std::fs::read_dir(root.join("anims"))
        .expect("anims dir")
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_stem()
                .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case(&cref.stem))
        })
        .expect("clip ships");
    let clip =
        caer_assets::nif::read_clip(&std::fs::read(&path).expect("read")).expect("clip parses");

    let scale = 1.30_f32;
    let offset = 17.5_f32;
    let zero = caer_render::entities::posed_head_height(&skel, &clip, 0.0, scale, 0.0)
        .expect("head landmark");
    let anchored = caer_render::entities::posed_head_height(&skel, &clip, 0.0, scale, offset)
        .expect("head landmark");

    // KNOWN-BAD: dropping the offset lowers the landmark by exactly `offset * scale`. If these two
    // ever agree, the argument has stopped reaching the computation.
    let expected = offset * scale;
    assert!(
        (anchored - zero - expected).abs() < 1e-2,
        "z_offset must raise the landmark by offset * scale ({expected:.2}), got {:.2}",
        anchored - zero
    );
    // And the offset must scale WITH the instance, not be added afterwards.
    let at_unit = caer_render::entities::posed_head_height(&skel, &clip, 0.0, 1.0, offset)
        .expect("head landmark");
    let at_unit_zero =
        caer_render::entities::posed_head_height(&skel, &clip, 0.0, 1.0, 0.0).expect("head");
    assert!(
        ((at_unit - at_unit_zero) - offset).abs() < 1e-2,
        "at unit scale the offset must appear undivided, got {:.2}",
        at_unit - at_unit_zero
    );
}

/// The pre-world camera must not move when the race does.
///
/// Measured across the 24 Eden character-create captures: mask the subject column and both UI
/// panels, and the scenery that remains agrees between any two captures of one realm while
/// differing sharply between realms. A camera composed around the figure cannot produce that, so
/// Eden's is fixed per realm and the subject's dimensions are not inputs to it.
///
/// Regenerate with `caer audit backdrop <bank>`: 3 groups of 7/8/9 recovered from unlabelled
/// files, within-group max 4.00 against between-group min 56.00. The figures first recorded here
/// (0.00 / 62 / 96) came from an uncommitted in-session computation whose mask and realm mapping
/// were never written down; they are superseded, and the conclusion is unchanged.
///
/// This **replaces** an assertion that the lens should track `posed_head_height`. That policy was
/// recorded as a candidate, explicitly not as retail-correct, pending exactly this reference; the
/// reference arrived and falsified it. Head tracking is now the control: it is what the product
/// must NOT do.
///
/// Still exercises `entities::posed_head_height` over real client skeletons and clips, because the
/// control needs real per-race spread to be worth anything — a substituted constant would make the
/// control pass by collapsing rather than by being right.
///
/// **Input contract, not an end-to-end gate.** It passes `z_offset = 0`, resolves archives
/// directly, and never touches `EntityModels` assembly, the GPU palette upload, or the `rustdaoc`
/// camera wiring.
#[test]
fn the_preworld_camera_does_not_move_when_the_race_does() {
    use caer_assets::figures::FigureModels;
    use caer_render::preworld_scene;

    let root = root();
    // Hibernia, not Albion. The KNOWN-BAD control below feeds a measured head in as `aim_height`,
    // and `aim_height` is the LAST fallback — a realm that ships a solved eye fraction overrides
    // it, which pins the control's eye to one constant and leaves the test unable to tell a fixed
    // camera from a tracking one. It says so itself, and it went red on Albion the moment that
    // realm gained a solved eye. Hibernia ships no eye, so the anchor path is still live there.
    let Some(scene) = preworld_scene::load(&root, 3) else {
        panic!("realm 3 pre-world scene must load — REQ-025, set CAER_CLIENT");
    };
    let tables = caer_assets::anims::AnimTables::load(root.join("gamedata.mpk")).expect("anims");
    let mut archives: Vec<_> = std::fs::read_dir(root.join("figures/fig3"))
        .expect("fig3 dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().starts_with("sfig"))
        })
        .collect();
    archives.sort();

    // Instance scale through the same owner chain the renderer uses:
    // `EntityModels::race_display_scale` is `career::race_model` into `MonsterModels::model_scale`.
    // A literal here would pass even if that runtime source were wrong.
    let resolver =
        caer_assets::monsters::MonsterModels::load(root.join("gamedata.mpk")).expect("monsters");
    let display_scale = |race: u8, fig3_gender: u8| -> f32 {
        caer_protocol::career::race_model(race, fig3_gender.saturating_sub(1))
            .map(|mid| resolver.model_scale(mid))
            .unwrap_or(1.0)
    };

    // Kobold, Elf female (the case Matt reported) and Troll span the instance-scale range.
    let mut measured = Vec::new();
    let mut product = Vec::new();
    let mut control = Vec::new();
    for (race, gender) in [(8u8, 2u8), (11, 2), (6, 1)] {
        let scale = display_scale(race, gender);
        let name = FigureModels::race_name(race).expect("race name");
        let want = FigureModels::skeleton_member(race, gender).expect("skeleton member");
        let skel = archives
            .iter()
            .find_map(|a| {
                let ms = caer_assets::open(a).ok()?;
                let m = ms.iter().find(|m| m.name.eq_ignore_ascii_case(&want))?;
                caer_assets::nif::read_skeleton(&m.data).ok()
            })
            .unwrap_or_else(|| panic!("{name} gender {gender} ships a skeleton"));
        let gw = if gender == 2 { "Female" } else { "Male" };
        let cref = tables
            .set_for_race_gender(name, gw)
            .and_then(|set| tables.clip(set, caer_assets::anims::Action::Idle))
            .unwrap_or_else(|| panic!("{name} {gw} resolves an idle clip"));
        let path = std::fs::read_dir(root.join("anims"))
            .expect("anims dir")
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_stem()
                    .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case(&cref.stem))
            })
            .unwrap_or_else(|| panic!("{} ships", cref.stem));
        let clip = caer_assets::nif::read_clip(&std::fs::read(&path).expect("read clip"))
            .expect("clip parses");

        // The same helper rustdaoc's camera path calls, over real client data.
        let head = caer_render::entities::posed_head_height(&skel, &clip, 0.0, scale, 0.0)
            .unwrap_or_else(|| panic!("{name} {gw}: no posed head height"));
        assert!(
            head.is_finite() && head > 1.0,
            "{name} {gw}: implausible head height {head}"
        );
        measured.push((format!("{name} {gw}"), head));

        // The product camera. `aim_preworld_camera` calls exactly this and passes no subject, so
        // every race must get the same lens.
        let (eye, focus) = preworld_scene::framing(&scene, 16.0 / 9.0);
        product.push((format!("{name} {gw}"), eye, focus));

        // KNOWN-BAD CONTROL: the head-tracking policy Eden falsified. Feeding the measured head in
        // moves the lens per race, which would shift the backdrop between two races — the one
        // thing the reference captures rule out.
        let (bad_eye, _, _) = preworld_scene::framing_around_character(
            &scene,
            preworld_scene::CHARACTER_HEIGHT,
            Some(head),
            16.0 / 9.0,
        );
        control.push((format!("{name} {gw}"), bad_eye.z));
    }

    // A constant, or a fraction of a near-constant `model_height`, would collapse this spread.
    let lo = measured.iter().map(|(_, h)| *h).fold(f32::MAX, f32::min);
    let hi = measured.iter().map(|(_, h)| *h).fold(f32::MIN, f32::max);
    assert!(
        hi - lo > 10.0,
        "head heights must vary with race and instance scale, got {measured:?} — a substituted \
         constant would satisfy every assertion above this one"
    );

    // THE GATE: one lens for every race, matching Eden's pixel-identical backdrop.
    let (_, eye0, focus0) = product[0];
    for (who, eye, focus) in &product {
        assert!(
            (eye.x - eye0.x).abs() < 1e-3
                && (eye.y - eye0.y).abs() < 1e-3
                && (eye.z - eye0.z).abs() < 1e-3
                && (focus.z - focus0.z).abs() < 1e-3,
            "{who}: the pre-world lens moved with the race — eye {eye:?} focus {focus:?} against \
             {eye0:?} / {focus0:?}. Eden's backdrop is pixel-identical across races in a realm, so \
             any per-race camera term is a defect"
        );
    }

    // THE CONTROL, seen red: head tracking must move the eye across this scale range. If it did
    // not, the gate above would be passing for the wrong reason — an instrument that cannot tell
    // the two policies apart proves nothing about which one the product uses.
    let c_lo = control.iter().map(|(_, z)| *z).fold(f32::MAX, f32::min);
    let c_hi = control.iter().map(|(_, z)| *z).fold(f32::MIN, f32::max);
    assert!(
        c_hi - c_lo > 10.0,
        "the control must move the eye with instance scale, got {control:?} — without that spread \
         this test cannot distinguish a fixed camera from a tracking one"
    );
    println!("  measured head heights: {measured:?}");
    println!("  product lens (must be one): {product:?}");
    println!("  control eye height by race: {control:?}");
}

/// `framing_around_character` pulls the camera back by `h * 1.5 / tan(fov/2)`, so for any `h` the
/// ratio of body height to camera distance is `tan(fov/2) / 1.5`. That is a property of the
/// function and this test asserts it.
///
/// **It is not a statement about the screen, and an earlier version of this test said it was.**
/// The claim "every race is drawn at the same apparent size" was written from this algebra alone
/// and is false: `stand_preworld_avatar` hands the camera `EntityModels::model_height`, which is
/// the *unscaled* mesh height, while the per-race scale (Kobold 0.70 … Troll 1.30) is applied only
/// to the instance. So `h` barely varies, the camera sits at a near-constant distance, and
/// rendered size scales with the race after all — measured at 1024x768: Kobold 178px, Dwarf 193,
/// Frostalf 254, Norseman 279, Troll 340, with height/scale near-constant at 254-261.
///
/// Ledger D4 and the row about the unscaled framing height carry that measurement. This test
/// stays because the formula is worth pinning; it just may not be read as a claim about pixels.
#[test]
fn framing_distance_is_proportional_to_the_height_it_is_given() {
    use caer_render::preworld_scene;

    let Some(scene) = preworld_scene::load(&root(), 1) else {
        panic!("realm 1 pre-world scene must load — REQ-025, set CAER_CLIENT");
    };

    let nominal = preworld_scene::CHARACTER_HEIGHT;
    let mut ratios = Vec::new();
    for scale in [0.70_f32, 0.75, 1.0, 1.09, 1.30] {
        let h = nominal * scale;
        let (eye, focus, feet) =
            preworld_scene::framing_around_character(&scene, h, None, 16.0 / 9.0);
        let back = ((eye.x - feet.x).powi(2) + (eye.y - feet.y).powi(2)).sqrt();
        assert!(
            back > 1.0,
            "scale {scale}: camera sat on top of the subject"
        );
        // The fractions are per realm now: Albion ships a solved eye of 0.630, and only the
        // realms with none fall back to the generic 0.62 / 0.55. Read them rather than restating
        // them — a literal here would have to be edited every time a camera is re-solved, and
        // "the test needs updating" is how a real regression gets waved through.
        let ship = caer_render::preworld_camera_tune::shipped(scene.realm);
        assert!(
            ((eye.z - feet.z) / h - ship.eye.unwrap_or(0.62)).abs() < 1e-3,
            "scale {scale}: eye height is not a fixed fraction of the height given"
        );
        assert!(
            ((focus.z - feet.z) / h - ship.focus.unwrap_or(0.55)).abs() < 1e-3,
            "scale {scale}: focus height is not a fixed fraction of the height given"
        );
        ratios.push((scale, h / back));
    }

    let first = ratios[0].1;
    for (scale, r) in &ratios {
        assert!(
            (r - first).abs() < 1e-4,
            "scale {scale}: height/distance {r} differs from {first} — the framing formula changed"
        );
    }
    println!(
        "framing height/distance is {first:.6} for every height passed. Whether the CALLER varies \
         that height is a separate question, and today it does not (ledger D4)."
    );
}

/// **The capture path is deterministic, and it is sensitive.**
///
/// Every visual claim in this project rests on two captures differing only when their state
/// differs. That was assumed, never shown — and it is the assumption that makes a pixel comparison
/// mean anything at all. Both halves are needed: a path that always returns the same bytes would
/// pass the first check and prove nothing.
///
/// This is the calibration for `rustdaoc --preworld … --screenshot`. It renders through the same
/// GPU composition the headless capture uses, at a small size so it stays cheap.
#[test]
fn the_capture_path_is_deterministic_and_sensitive() {
    use caer_render::camera::Camera;
    use caer_render::entities::EntityModels;
    use glam::Vec3;

    let root = root();
    // SAFETY: single-threaded test entry; `EntityModels::load` reads the client root from env.
    std::env::set_var("CAER_CLIENT", &root);
    let (w, h) = (256u32, 192u32);
    let mut gpu = pollster::block_on(Gpu::new_headless(w, h, 60_000.0)).expect("gpu init");
    let mut models = EntityModels::load().expect("entity models");
    // Same framing the equipment pixel proof uses: a body-height subject filling the frame. Without
    // a camera both frames are the clear colour and the determinism half passes vacuously — which
    // is exactly what the sensitivity half caught the first time this ran.
    let camera = Camera::new(
        Vec3::new(63.0, -180.0, 63.0),
        Vec3::new(0.0, 0.0, 55.0),
        Vec3::ZERO,
        w as f32 / h as f32,
    );
    gpu.set_view_proj(camera.view_proj().to_cols_array_2d());

    let mut shoot = |race: u8, gender: u8| -> Vec<u8> {
        let id = models
            .ensure_avatar(
                &mut gpu,
                race,
                gender,
                caer_protocol::customization::Customization::default(),
                None,
            )
            .expect("avatar");
        let rig = models.skinned_rig(id).expect("rig");
        let palette = rig.palette_of(&rig.clip, 0.0);
        gpu.clear_entity_instances();
        gpu.clear_skinned_instances();
        gpu.update_skinned_instances(
            id,
            &[caer_render::gpu::SkinnedInstance {
                pos_yaw: [0.0, 0.0, 0.0, 0.0],
                scale: 1.0,
                palette_base: 0.0,
            }],
            &palette,
        );
        gpu.render_to_rgba(0).expect("GPU wait")
    };

    // Same inputs twice: byte-identical, or no pixel comparison anywhere in this project means
    // anything.
    let a = shoot(1, 1);
    let b = shoot(1, 1);
    assert_eq!(
        a.len(),
        (w * h * 4) as usize,
        "unexpected frame size — the capture path changed shape"
    );
    assert!(
        a == b,
        "two captures of identical state differ; every visual comparison built on this path is \
         unreliable until that is explained"
    );

    // A different subject: the frame must change, or the path is returning something constant and
    // the check above is vacuous.
    let c = shoot(6, 1); // Troll — a very different body from a Briton.
    let changed = a
        .chunks_exact(4)
        .zip(c.chunks_exact(4))
        .filter(|(x, y)| x[..3] != y[..3])
        .count();
    assert!(
        changed > 0,
        "a Briton and a Troll rendered to the same bytes — the capture path is not sensitive to \
         its inputs, so the determinism check above proves nothing"
    );
    println!(
        "capture path: identical inputs are byte-identical; a different race changes {changed} of \
         {} pixels",
        w * h
    );
}

/// **Does the character actually stand on the ground?**
///
/// `subject_projects_to_screen_centre` cannot answer this. It compares the subject against a camera
/// that was aimed at the subject, so the two agree by construction — it can catch a projection-maths
/// bug and nothing else. Ledger A14 ("character stands in the wrong place, separate from camera
/// aim") is invisible to it.
///
/// This asks the scene instead: cast down from the `collidee` anchor and report where the surface
/// is. No reference capture needed — a body floating over the terrain or buried in it is wrong on
/// the scene's own terms.
#[test]
fn the_subject_anchor_sits_on_the_scene_surface() {
    use caer_render::scene_census;

    let root = root();
    let mut report = Vec::new();
    for (realm, name) in [(1u8, "Albion"), (2, "Midgard"), (3, "Hibernia")] {
        let Some(scene) = caer_render::preworld_scene::load(&root, realm) else {
            panic!("realm {realm} scene must load — REQ-025, set CAER_CLIENT");
        };
        let a = scene.subject_anchor;
        // Probe from well above the anchor so the ground below it is found even if the anchor
        // itself is buried.
        let p = scene_census::surface_under(&scene, a.x, a.y, a.z + 10_000.0);
        let drop = p.below.map(|s| a.z - s);
        println!(
            "{name:<9} anchor ({:.0}, {:.0}, {:.0})  surface below={:?}  above={:?}  triangles under it={}  anchor is {:?} above the surface",
            a.x,
            a.y,
            a.z,
            p.below.map(|v| v.round()),
            p.above.map(|v| v.round()),
            p.hits,
            drop.map(|v| v.round())
        );
        let c = scene_census::census(&scene);
        let owner = p
            .below_part
            .and_then(|i| c.parts.get(i))
            .and_then(|q| q.texture.clone())
            .unwrap_or_else(|| "<unknown>".into());
        println!("{name:<9}   the surface under it belongs to: {owner}");
        report.push((name, p.hits, drop, owner));
    }

    for (name, hits, drop, owner) in &report {
        assert!(
            *hits > 0,
            "{name}: nothing in the scene lies under the subject anchor — the character stands over \
             a hole, and no framing measurement would notice"
        );
        let Some(d) = drop else {
            panic!(
                "{name}: no surface at or below the anchor — the character has nothing to stand on"
            );
        };
        // A body standing on ground is within a stride of it. Ten units either way is generous at
        // these scene scales (stages are 236-1444 units across) and still catches a body parked
        // hundreds of units into the air or under the terrain.
        // The thing under the anchor must be scene art, not a marker quad placed at the anchor —
        // otherwise this measures itself.
        assert_ne!(
            owner, "<unknown>",
            "{name}: the surface under the anchor belongs to no draw range"
        );
        assert!(
            d.abs() < 10.0,
            "{name}: the subject anchor sits {d:.0} units off the surface beneath it — the \
             character is not standing on the ground it is drawn over"
        );
    }
}
