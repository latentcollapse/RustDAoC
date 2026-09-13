//! H5 measurement — what a pre-world screen transition actually costs on the event thread.
//!
//! `PreWorldHud::ensure_loaded` is called from `render`, which runs inside winit's
//! `RedrawRequested` on the main thread. Anything it spends is time the window is not answering
//! the compositor. The batch asserts this "explains multi-second transitions"; this test measures
//! it instead of inheriting the claim, and separates two distinct costs:
//!
//! * **cold** — first visit to a screen, when its archives are read and decoded.
//! * **warm** — every subsequent frame on that same screen. This must be ~0. Anything above noise
//!   means a load is being retried per frame, because `load_*` caches successes only and a failed
//!   or absent member is re-read (and its archive fully re-inflated) on every redraw.
//!
//! GPU-free: `ensure_loaded` decodes into CPU images; upload happens later in `render`.
//!
//! Requires `CAER_CLIENT`: a transition measurement without the retail assets is not evidence.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use caer_render::preworld::{PreWorldHud, PreWorldScreen};

/// One 60 Hz frame. Any per-frame cost at or above this makes the screen miss its budget alone.
const FRAME_BUDGET: Duration = Duration::from_micros(16_667);

fn client_root() -> PathBuf {
    let root = caer_assets::client_dep::required_caer_client_root("preworld_transition_cost");
    assert!(
        root.join("pregame").is_dir(),
        "CAER_CLIENT has no pregame directory: {} — this transition-cost measurement requires real \
         assets. REQ-025: a test that cannot run must not report pass.",
        root.display()
    );
    root
}

/// Screens whose assets come from the retail tree.
const SCREENS: [(&str, PreWorldScreen); 5] = [
    ("splash", PreWorldScreen::Splash),
    ("login", PreWorldScreen::Login),
    ("realmselect", PreWorldScreen::RealmSelect),
    ("charselect", PreWorldScreen::CharSelect),
    ("charcreate", PreWorldScreen::CharCreate),
];

/// Warm frames to average over. Large enough that a sub-millisecond per-frame retry still shows.
const WARM_FRAMES: u32 = 120;

#[test]
fn preworld_screen_transition_cost_is_measured_and_warm_frames_are_free() {
    let root = client_root();

    let mut offenders: Vec<String> = Vec::new();
    println!("\n  screen        cold        warm/frame   verdict");
    println!("  ------------------------------------------------------");

    for (name, screen) in SCREENS {
        let mut hud = PreWorldHud::new(root.clone());
        hud.set_screen(screen);

        let t0 = Instant::now();
        let cold_result = hud.ensure_loaded();
        let cold = t0.elapsed();

        // Same screen, repeatedly — exactly what `render` does every frame while it is displayed.
        let t1 = Instant::now();
        for _ in 0..WARM_FRAMES {
            let _ = hud.ensure_loaded();
        }
        let warm = t1.elapsed() / WARM_FRAMES;

        // A cached screen should cost a few map lookups: microseconds, not milliseconds.
        let leaking = warm >= Duration::from_micros(200);
        let verdict = if leaking {
            "PER-FRAME RELOAD"
        } else if cold >= FRAME_BUDGET {
            "cold stall only"
        } else {
            "ok"
        };
        println!(
            "  {name:<12}{:>8.2} ms{:>12.3} ms   {verdict}{}",
            cold.as_secs_f64() * 1000.0,
            warm.as_secs_f64() * 1000.0,
            if cold_result.is_err() {
                "  (load error)"
            } else {
                ""
            }
        );

        if leaking {
            offenders.push(format!(
                "{name}: {:.3} ms every frame while displayed",
                warm.as_secs_f64() * 1000.0
            ));
        }
    }
    println!();

    assert!(
        offenders.is_empty(),
        "pre-world screens re-load assets on every frame (H5 per-frame retry):\n  - {}",
        offenders.join("\n  - ")
    );
}

/// What one member costs, whole-archive versus by directory offset.
///
/// A member lookup used to mean `caer_assets::open`: inflate **every** member, keep one, drop the
/// rest. `caer_assets::read_named` seeks to the member's own zlib stream instead. Both paths run
/// here on bytes already in memory, so the numbers are decode cost only and the file read (and its
/// page-cache state) cannot flatter either one.
///
/// The assertion is the point: if random access ever regresses to a full inflate, the ratio
/// collapses and this fails rather than quietly costing the player another 170 ms per screen.
#[test]
fn single_member_fetch_beats_full_archive_inflate() {
    let root = client_root();
    println!("\n  archive                        members   full inflate   one member   ratio");
    println!("  ---------------------------------------------------------------------------");

    let mut checked = 0;
    for rel in [
        "pregame/pregame.mpk",
        "pregame/splash.mpk",
        "pregame/realmdesc.mpk",
        "data/loading/alb1.mpk",
    ] {
        let path = root.join(rel);
        if !path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path).expect("read archive");

        let t = Instant::now();
        let entries = caer_assets::read(&bytes).expect("full inflate");
        let full = t.elapsed();

        // The last member is the worst case for a sequential walk and the best demonstration that
        // the seek does not depend on position.
        let last = entries.last().expect("archive has members").name.clone();
        let t = Instant::now();
        let one = caer_assets::read_named(&bytes, &[last.as_str()]).expect("single member");
        let single = t.elapsed();
        assert_eq!(one.len(), 1, "{rel}: {last} not fetched");

        let ratio = full.as_secs_f64() / single.as_secs_f64().max(f64::EPSILON);
        println!(
            "  {rel:<30}{:>7}   {:>9.2} ms   {:>8.2} ms   {ratio:>5.1}x",
            entries.len(),
            full.as_secs_f64() * 1000.0,
            single.as_secs_f64() * 1000.0,
        );

        // Only meaningful on multi-member archives; a one-member archive is its own worst case.
        if entries.len() >= 8 {
            assert!(
                ratio >= 4.0,
                "{rel}: fetching one of {} members cost {:.2} ms against {:.2} ms for the whole \
                 archive ({ratio:.1}x) — random access has regressed to a full inflate",
                entries.len(),
                single.as_secs_f64() * 1000.0,
                full.as_secs_f64() * 1000.0,
            );
            checked += 1;
        }
    }
    println!();
    assert!(checked > 0, "no multi-member archive was measured");
}
