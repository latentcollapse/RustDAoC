//! Golden-image test for the 2D overlay layer (C.1).
//!
//! The unit tests in `hud.rs` prove the HUD *tessellates* without panicking. They cannot prove it
//! ever reaches the framebuffer — a wrong pipeline format, a missing texture upload or a botched
//! render pass would leave every one of them green while the screen stayed empty. This test runs
//! the real headless GPU path (`render_to_rgba_with_overlay`) end to end and looks at the pixels.
//!
//! Unlike `terrain_golden.rs` it needs **no client assets and no server**: the scene is an empty
//! headless frame and the HUD is driven from a fixed [`HudState`], so it runs anywhere.
//!
//! Refresh after an intentional visual change:
//!
//! ```bash
//! CAER_UPDATE_GOLDENS=1 cargo test -p caer-render --test hud_golden
//! ```
//!
//! …then **look at the PNG**. A golden refreshed without being viewed launders a regression into
//! the baseline.

use std::path::{Path, PathBuf};

use caer_protocol::status::PlayerStatus;
use caer_render::gpu::Gpu;
use caer_render::hud::{self, HudState, TargetInfo};
use caer_render::ui;

const W: u32 = 800;
const H: u32 = 450;

fn goldens_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens")
}

fn decode(bytes: &[u8]) -> (Vec<u8>, u32, u32) {
    let dec = png::Decoder::new(bytes);
    let mut reader = dec.read_info().expect("png header");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("png data");
    buf.truncate(info.buffer_size());
    (buf, info.width, info.height)
}

fn encode(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(std::io::Cursor::new(&mut out), W, H);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .expect("png header")
            .write_image_data(rgba)
            .expect("png data");
    }
    out
}

/// Fraction of pixels differing appreciably. Same metric as `terrain_golden.rs`, for the same
/// measured reason: a mean is too blunt to catch a localized change, and the HUD *is* localized.
fn changed_fraction(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "buffers differ in size");
    let px = a.len() / 4;
    let changed = (0..px)
        .filter(|i| (0..4).any(|c| a[i * 4 + c].abs_diff(b[i * 4 + c]) > 8))
        .count();
    changed as f64 / px as f64
}

/// The state the golden is rendered from: a damaged caster with a target. Values are the real
/// capture numbers used throughout the 0xAD tests, so the golden shows plausible in-game figures.
fn golden_state() -> HudState {
    HudState {
        name: "Feile".into(),
        status: PlayerStatus {
            health: 1848,
            max_health: 2004,
            health_pct: 92,
            mana: 312,
            max_mana: 492,
            mana_pct: 63,
            endurance: 95,
            max_endurance: 100,
            endurance_pct: 95,
            concentration: 378,
            max_concentration: 378,
            concentration_pct: 100,
            sitting: false,
        },
        target: Some(TargetInfo {
            name: "a large spider".into(),
            health_pct: 43,
        }),
        cast_bar: None,
        fps: 60,
        ..HudState::default()
    }
}

/// Render one headless frame, optionally with the HUD composited on top.
fn render(hud_state: Option<&HudState>) -> Vec<u8> {
    let mut gpu = pollster::block_on(Gpu::new_headless(W, H, 1000.0)).expect("gpu init");
    let overlay = hud_state.map(|s| ui::tessellate_headless([W, H], 1.0, |u| hud::build(u, s)));
    // `count = 0`: no entity instances. The scene is deliberately empty so this test measures the
    // overlay and nothing else.
    gpu.render_to_rgba_with_overlay(0, overlay)
        .expect("GPU wait")
}

/// The load-bearing test: the overlay must actually change the framebuffer.
///
/// This is the "prove a golden can fail" step — if compositing silently did nothing, the golden
/// below would happily bake an empty frame and pass forever.
#[test]
fn the_overlay_reaches_the_framebuffer() {
    let bare = render(None);
    let with_hud = render(Some(&golden_state()));

    let changed = changed_fraction(&bare, &with_hud);
    assert!(
        changed > 0.01,
        "the HUD changed only {:.3}% of pixels — the overlay is not reaching the framebuffer",
        changed * 100.0,
    );

    // …and an empty-scene render must be deterministic, so the metric above means what it says.
    assert_eq!(
        changed_fraction(&bare, &render(None)),
        0.0,
        "headless render is not deterministic"
    );
}

/// A HUD with no target must draw strictly less than one with a target — the target frame is
/// conditional, and a regression that always draws it (or never does) would be invisible to the
/// panic-free unit tests.
#[test]
fn the_target_frame_is_conditional() {
    let mut no_target = golden_state();
    no_target.target = None;

    let bare = render(None);
    let without = changed_fraction(&bare, &render(Some(&no_target)));
    let with = changed_fraction(&bare, &render(Some(&golden_state())));

    assert!(without > 0.0, "the player frame should always draw");
    assert!(
        with > without,
        "adding a target should paint more pixels ({with:.4} vs {without:.4})"
    );
}

/// The health bar's filled width must actually track the health fraction.
///
/// Everything else here is a "did anything draw" check, which a bar stuck at 100% would pass. This
/// measures the widest run of health-red pixels on any scanline and asserts it scales with the
/// fraction — cheap, and it pins the one number the whole feature exists to communicate.
#[test]
fn the_health_bar_width_tracks_the_health_fraction() {
    /// Widest horizontal run of pixels close to the health colour (178, 34, 34).
    fn widest_red_run(rgba: &[u8]) -> u32 {
        let mut best = 0;
        for y in 0..H as usize {
            let (mut run, mut row_best) = (0u32, 0u32);
            for x in 0..W as usize {
                let p = (y * W as usize + x) * 4;
                let is_red = rgba[p].abs_diff(178) < 40
                    && rgba[p + 1].abs_diff(34) < 40
                    && rgba[p + 2].abs_diff(34) < 40;
                run = if is_red { run + 1 } else { 0 };
                row_best = row_best.max(run);
            }
            best = best.max(row_best);
        }
        best
    }

    let measure = |pct: u8, cur: u16, max: u16| {
        let mut s = golden_state();
        s.target = None; // the target frame is red too — keep one bar in the frame
        s.status.health = cur;
        s.status.max_health = max;
        s.status.health_pct = pct;
        widest_red_run(&render(Some(&s)))
    };

    let full = measure(100, 200, 200);
    let half = measure(50, 100, 200);
    let quarter = measure(25, 50, 200);

    assert!(
        full > 150,
        "a full health bar should span most of its 180px width, got {full}"
    );
    // Proportional within a couple of pixels of rounding either way.
    assert!(
        half.abs_diff(full / 2) <= 3,
        "half health drew {half}px, expected about {} (full was {full})",
        full / 2
    );
    assert!(
        quarter.abs_diff(full / 4) <= 3,
        "quarter health drew {quarter}px, expected about {} (full was {full})",
        full / 4
    );
}

/// The full overlay stack (C.7): nameplates + floating damage + combat log + HUD, all in one
/// frame, so their layering and legibility can be reviewed together.
#[test]
fn golden_overlay_full() {
    use caer_render::combat::{CombatLog, Damage, FloatingTexts};
    use caer_render::nameplates::Nameplate;
    use caer_world::Kind;

    let hud_state = golden_state();

    let plates = vec![
        Nameplate {
            screen: [300.0, 150.0],
            dist2: 400.0 * 400.0,
            name: "a giant skeleton".into(),
            subtitle: String::new(),
            kind: Kind::Npc,
            health_pct: 43,
            is_target: true,
        },
        Nameplate {
            screen: [560.0, 190.0],
            dist2: 1500.0 * 1500.0,
            name: "Lilillyn".into(),
            subtitle: "Clan Cotswold".into(),
            kind: Kind::Player,
            health_pct: 100,
            is_target: false,
        },
        Nameplate {
            screen: [180.0, 230.0],
            dist2: 4800.0 * 4800.0,
            name: "Disciple Imogen".into(),
            subtitle: "Guardian".into(),
            kind: Kind::Npc,
            health_pct: 100,
            is_target: false,
        },
    ];

    let mut log = CombatLog::new();
    for (t, s) in [
        (0x1eu8, "You target [a giant skeleton]."),
        (
            0x11,
            "You attack the giant skeleton with your sword and hit for 63 damage!",
        ),
        (
            0x11,
            "You critical hit the giant skeleton for an additional 40 damage!",
        ),
        (0x1d, "The giant skeleton hits your leg for 10 (-8) damage!"),
        (0x1a, "The giant skeleton drops a bag of coins."),
    ] {
        log.push(t, s);
    }

    let mut floaters = FloatingTexts::new();
    floaters.spawn(
        Damage {
            amount: 63,
            modifier: 0,
            incoming: false,
            critical: false,
            provenance: caer_render::combat::CombatProvenance::ChatMessage,
        },
        [300.0, 210.0],
    );
    floaters.spawn(
        Damage {
            amount: 40,
            modifier: 0,
            incoming: false,
            critical: true,
            provenance: caer_render::combat::CombatProvenance::ChatMessage,
        },
        [344.0, 240.0],
    );
    floaters.spawn(
        Damage {
            amount: 10,
            modifier: -8,
            incoming: true,
            critical: false,
            provenance: caer_render::combat::CombatProvenance::ChatMessage,
        },
        [400.0, 300.0],
    );

    // Quickbar filled from a real Paladin's skill list (the names decoded out of the captures).
    let mut bar = caer_render::quickbar::Quickbar::new();
    {
        use caer_protocol::skills::{Skill, SkillKind};
        let sk = |name: &str, kind| Skill {
            level: 5,
            internal_id: 1,
            kind,
            bonus: 0,
            icon: 0,
            name: name.into(),
        };
        bar.autofill(&[
            sk("Slash", SkillKind::Specialization), // a rating — must NOT take a slot
            sk("Sprint", SkillKind::Ability),
            sk("Slam", SkillKind::Style),
            sk("Cure Poison", SkillKind::Spell),
            sk("Weaponry: Slashing", SkillKind::Ability),
            sk("Purge", SkillKind::RealmAbility),
        ]);
    }

    // Chat box open, mid-typing, to show the interactive layer.
    let mut chat = caer_render::chat::ChatState::new();
    chat.open();
    chat.input = "well met".into();

    let mut gpu = pollster::block_on(Gpu::new_headless(W, H, 1000.0)).expect("gpu init");
    let overlay = ui::tessellate_headless([W, H], 1.0, |u| {
        caer_render::nameplates::draw(u, &plates);
        caer_render::combat::draw_floating(u, &floaters);
        caer_render::combat::draw_log(u, &log);
        caer_render::quickbar::draw(u, &bar);
        hud::build(u, &hud_state);
        caer_render::chat::draw(u, &mut chat);
    });
    let actual = encode(
        &gpu.render_to_rgba_with_overlay(0, Some(overlay))
            .expect("GPU wait"),
    );

    let golden_path = goldens_dir().join("overlay_full.png");
    if std::env::var_os("CAER_UPDATE_GOLDENS").is_some() || !golden_path.exists() {
        std::fs::create_dir_all(goldens_dir()).ok();
        std::fs::write(&golden_path, &actual).expect("write golden");
        eprintln!(
            "golden overlay_full written to {} — REVIEW IT",
            golden_path.display()
        );
        return;
    }

    let (a, ..) = decode(&actual);
    let (g, ..) = decode(&std::fs::read(&golden_path).expect("read golden"));
    let changed = changed_fraction(&a, &g);
    if changed >= 0.001 {
        let out = std::env::temp_dir().join("caer_golden_overlay_full_actual.png");
        std::fs::write(&out, &actual).ok();
        panic!(
            "overlay_full: {:.2}% of pixels changed. Rendered frame kept at {}.",
            changed * 100.0,
            out.display()
        );
    }
}

#[test]
fn golden_hud() {
    let actual = encode(&render(Some(&golden_state())));
    let golden_path = goldens_dir().join("hud.png");

    if std::env::var_os("CAER_UPDATE_GOLDENS").is_some() || !golden_path.exists() {
        std::fs::create_dir_all(goldens_dir()).ok();
        std::fs::write(&golden_path, &actual).expect("write golden");
        eprintln!(
            "golden hud written to {} — REVIEW IT before committing",
            golden_path.display()
        );
        return;
    }

    let (a, aw, ah) = decode(&actual);
    let (g, gw, gh) = decode(&std::fs::read(&golden_path).expect("read golden"));
    assert_eq!((aw, ah), (gw, gh), "hud: frame size changed");

    let changed = changed_fraction(&a, &g);
    if changed >= 0.001 {
        let out = std::env::temp_dir().join("caer_golden_hud_actual.png");
        std::fs::write(&out, &actual).ok();
        panic!(
            "hud: {:.2}% of pixels changed — the overlay regressed.\n\
             Rendered frame kept at {} for inspection.\n\
             If intentional: CAER_UPDATE_GOLDENS=1 cargo test -p caer-render --test hud_golden, then LOOK at the PNG.",
            changed * 100.0,
            out.display(),
        );
    }
}
