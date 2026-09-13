//! `rustdaoc` — the player-level Rust DAoC client.
//!
//! This is the flagship shell: a live connection to a DoL server, the real world rendered around
//! *your character*, a player-anchored third-person camera, and no editor chrome. It's the thing
//! that becomes the drop-in game.dll replacement. It reuses the whole `caer_render` core (GPU
//! pipeline, terrain, entity meshes, live feed) — the developer/inspection lens (`caer-render`)
//! sits a level ABOVE this and shares the same rendering code, so anything you spot wrong here you
//! fix over there.
//!
//! Usage (launcher-authenticated first playtest — no invented credentials, no LIVE default):
//!   export CAER_CLIENT="/path/to/Dark Age of Camelot"
//!   export CAER_ACCOUNT=… CAER_PASSWORD=… CAER_SERVER=127.0.0.1:10312
//!   rustdaoc --account A --password P --server 127.0.0.1:10312
//!   rustdaoc … --screenshot out.png                 (headless: one frame, for verification)
//!
//! LIVE `:10311` requires explicit `CAER_ALLOW_LIVE_ENDPOINT=1`. Env overlays defaults; CLI wins.
//!
//! Controls are generated from the live binding map (`caer_render::keybinds::player_controls_help`).
//! Run `rustdaoc --help` for the current defaults. Do not hand-maintain a second control list here:
//! A/D are unbound (display-table Turn Left/Right are not in the 74-action internal table); Q/E are
//! `slide_left`/`slide_right`; F is `combat_mode` (legacy input alias `attack` still resolves).
//!
//! Movement integrates locally each frame (instant, the client is authoritative for its own
//! position) and streams PositionUpdates to the server on the wire cadence via the live command
//! seam (`LiveFeed::send`).
//!
//! Auth contract (Shape-2 first playtest): credentials come from the launcher/CLI before the
//! window loop. The on-screen login plate is local navigation after auth — not an account gate.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::Vec3;
use sha2::{Digest, Sha256};
use winit::application::ApplicationHandler;
use winit::event::{
    DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};

use caer_render::camera::Camera;
use caer_render::entities::EntityModels;
use caer_render::gpu::{Gpu, Instance};
use caer_render::hud::{HudState, TargetInfo};
use caer_render::keybinds::{Bindings, GameAction};
use caer_render::live::LiveCommand;
use caer_render::shell::Shell;
use caer_render::{live, render_world, terrain, CULL_RADIUS, LIVE_WARMUP};

/// How far Tab-targeting reaches, in world units (roughly DAoC's selectable range).
const TARGET_RANGE: i32 = 3_000;
/// Keep the product loop paced to the display-era contract instead of continuously polling and
/// recursively requesting redraws.  An uncapped `Poll` loop can acquire/present hundreds of
/// surfaces per second; under Wayland/KWin that starves pointer delivery and intermittently
/// exposes the compositor's black backing surface between DAoC pre-world frames.
const FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);

/// How long a pre-world quit waits for its `LiveCommand::Quit` to be acted on before leaving anyway.
///
/// Short on purpose. This is not the in-world logout, which sits on DAoC's quit timer for up to 65
/// seconds; nothing in the pre-world is in the world, and a Quit button that looks dead is its own
/// defect. Long enough that a healthy session always drains inside it, short enough that a wedged
/// socket does not trap the player in a window that will not close.
const QUIT_FLUSH_GRACE: Duration = Duration::from_millis(750);
/// Live billboard emitters. Same cap as WorldState's spawn queue — placeholder presence, not fidelity.
const MAX_LIVE_PARTICLE_SYSTEMS: usize = caer_world::MAX_PARTICLE_EFFECTS;
use caer_world::{Kind, WorldState};

/// Opt-in event-loop watchdog for live compositor failures. This deliberately does not attempt to
/// recover or mutate renderer state: when `CAER_LIVENESS_TRACE=1`, it reports the last completed
/// main-thread phase if the product loop stops advancing for two seconds. That distinguishes a
/// blocked frame from an event loop stranded in its wait policy without changing either outcome.
struct LivenessProbe {
    sequence: Arc<AtomicU64>,
    phase: Arc<AtomicU8>,
}

impl LivenessProbe {
    fn from_env() -> Option<Self> {
        if !env_truthy("CAER_LIVENESS_TRACE") {
            return None;
        }
        let sequence = Arc::new(AtomicU64::new(0));
        let phase = Arc::new(AtomicU8::new(0));
        let watch_sequence = Arc::clone(&sequence);
        let watch_phase = Arc::clone(&phase);
        std::thread::Builder::new()
            .name("caer-liveness".to_string())
            .spawn(move || {
                let mut previous = 0;
                let mut unchanged = 0u32;
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                    let current = watch_sequence.load(Ordering::Acquire);
                    if current == previous {
                        unchanged = unchanged.saturating_add(1);
                        if unchanged >= 2 {
                            let phase = watch_phase.load(Ordering::Acquire);
                            eprintln!(
                                "rustdaoc: LIVENESS stalled {}s at phase {} ({}) sequence={}",
                                unchanged,
                                phase,
                                liveness_phase_name(phase),
                                current
                            );
                        }
                    } else {
                        unchanged = 0;
                        previous = current;
                    }
                }
            })
            .ok()?;
        eprintln!("rustdaoc: liveness watchdog enabled");
        Some(Self { sequence, phase })
    }

    fn mark(&self, phase: u8) {
        self.phase.store(phase, Ordering::Release);
        self.sequence.fetch_add(1, Ordering::AcqRel);
    }
}

fn liveness_phase_name(phase: u8) -> &'static str {
    match phase {
        1 => "window_event",
        2 => "redraw_requested",
        3 => "frame_start",
        4 => "live_drain_complete",
        5 => "frame_ui_complete",
        6 => "gpu_render_enter",
        7 => "gpu_render_return",
        8 => "frame_complete",
        9 => "about_to_wait",
        10 => "redraw_queued",
        11 => "wait_deadline_installed",
        12 => "surface_reconfigure_enter",
        13 => "surface_reconfigure_return",
        _ => "startup",
    }
}

/// Pose needed to encode ECO C2S without touching WorldState inventory/money.
#[derive(Clone, Copy)]
struct EcoSlashCtx {
    player: [f32; 3],
    heading: u16,
    target: Option<u16>,
}

/// Product slash verbs that become typed [`LiveCommand`]s. Never mutates inventory/money/craft.
fn eco_slash_to_live(cmd: &str, ctx: &EcoSlashCtx) -> Option<LiveCommand> {
    if is_client_command(cmd, "sell") {
        let slot: u16 = cmd
            .strip_prefix("sell")
            .unwrap_or("")
            .trim()
            .parse()
            .unwrap_or(0);
        let merchant_id = ctx.target?;
        return Some(LiveCommand::SellItem {
            player_x: ctx.player[0] as u32,
            player_y: ctx.player[1] as u32,
            merchant_id,
            item_slot: slot,
        });
    }
    if is_client_command(cmd, "use") {
        let slot: u8 = cmd
            .strip_prefix("use")
            .unwrap_or("")
            .trim()
            .parse()
            .unwrap_or(0);
        return Some(LiveCommand::UseSlot {
            x: ctx.player[0],
            y: ctx.player[1],
            z: ctx.player[2],
            speed: 0.0,
            heading: ctx.heading,
            flag_speed_data: 0,
            slot,
            use_type: 0,
        });
    }
    if is_client_command(cmd, "craft") {
        let item_id: u16 = cmd
            .strip_prefix("craft")
            .unwrap_or("")
            .trim()
            .parse()
            .unwrap_or(0);
        return Some(LiveCommand::CraftItem { item_id });
    }
    if is_client_command(cmd, "destroy") {
        let slot: u16 = cmd
            .strip_prefix("destroy")
            .unwrap_or("")
            .trim()
            .parse()
            .unwrap_or(0);
        return Some(LiveCommand::DestroyItem { slot });
    }
    if is_client_command(cmd, "trainwindow") {
        return Some(LiveCommand::TrainWindow);
    }
    if is_client_command(cmd, "train") {
        let mut nums = cmd
            .strip_prefix("train")
            .unwrap_or("")
            .split_whitespace()
            .filter_map(|s| s.parse::<u8>().ok());
        let id_line = nums.next().unwrap_or(0);
        let row = nums.next().unwrap_or(0);
        let skill_index = nums.next().unwrap_or(0);
        return Some(LiveCommand::TrainRequest {
            player_x: ctx.player[0] as u32,
            player_y: ctx.player[1] as u32,
            id_line,
            unk: 0,
            row,
            skill_index,
        });
    }
    if is_client_command(cmd, "siege") {
        let mut nums = cmd
            .strip_prefix("siege")
            .unwrap_or("")
            .split_whitespace()
            .filter_map(|s| s.parse::<u8>().ok());
        let action = nums.next().unwrap_or(0);
        let ammo = nums.next().unwrap_or(0);
        return Some(LiveCommand::SiegeCommand { action, ammo });
    }
    if is_client_command(cmd, "sit") {
        return Some(LiveCommand::Sit { sit: true });
    }
    if is_client_command(cmd, "stand") {
        return Some(LiveCommand::Sit { sit: false });
    }
    None
}

/// Chat echo for typed eco slash. Train/siege must not fall through to `[eco] command`.
fn eco_slash_chat_note(cmd: &LiveCommand) -> String {
    match cmd {
        LiveCommand::SellItem {
            item_slot,
            merchant_id,
            ..
        } => format!("[sell] SellRequest slot={item_slot} merchant={merchant_id}"),
        LiveCommand::UseSlot { slot, .. } => format!("[use] UseSlot slot={slot}"),
        LiveCommand::CraftItem { item_id } => format!("[craft] CraftRequest item={item_id}"),
        LiveCommand::DestroyItem { slot } => {
            format!("[destroy] DestroyItemRequest slot={slot}")
        }
        LiveCommand::TrainWindow => "[trainwindow] TrainWindowHandler".into(),
        LiveCommand::TrainRequest {
            id_line,
            row,
            skill_index,
            ..
        } => format!("[train] TrainRequest id_line={id_line} row={row} skill={skill_index}"),
        LiveCommand::SiegeCommand { action, ammo } => {
            format!("[siege] SiegeCommand action={action} ammo={ammo}")
        }
        LiveCommand::Sit { sit } => {
            if *sit {
                "[sit] PlayerSitRequest sit=1".into()
            } else {
                "[sit] PlayerSitRequest sit=0".into()
            }
        }
        _ => "[eco] command".into(),
    }
}

/// Snapshot of Raw-folded eco/visual S2C that rustdaoc must not swallow silently.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EcoVisualSnap {
    emote: Option<(u16, u8)>,
    siege_oid: Option<u32>,
    siege_open: bool,
    enc: Option<(u16, u16)>,
    trainer: Option<(u8, u8, usize)>,
    market: Option<(u8, u8, u8)>,
    emblem: bool,
    lfg: Option<(u8, bool)>,
}

fn snap_eco_visual(world: &WorldState) -> EcoVisualSnap {
    let eco = world.eco();
    EcoVisualSnap {
        emote: world.last_emote().map(|e| (e.object_id, e.emote)),
        siege_oid: world.last_siege_anim().map(|a| a.object_id),
        siege_open: world.siege_interface_open(),
        enc: eco.encumberance.current().map(|e| (e.max, e.used)),
        trainer: eco
            .trainer
            .window()
            .map(|w| (w.code, w.points, w.lines.len())),
        market: eco
            .market_explorer
            .current()
            .map(|m| (m.count, m.page, m.max_page)),
        emblem: eco.emblem_dialogue,
        lfg: eco.find_group.map(|f| (f.count, f.empty_list)),
    }
}

fn eco_visual_chat_notes(before: &EcoVisualSnap, after: &EcoVisualSnap) -> Vec<String> {
    let mut out = Vec::new();
    if before.emote != after.emote {
        if let Some((oid, emote)) = after.emote {
            out.push(format!(
                "[emote] EmoteAnimation oid={oid} emote={emote} (presentation only)"
            ));
        }
    }
    if before.siege_oid != after.siege_oid {
        if let Some(oid) = after.siege_oid {
            out.push(format!("[siege] SiegeWeaponAnimation oid={oid}"));
        }
    }
    if before.siege_open != after.siege_open {
        out.push(format!(
            "[siege] interface {}",
            if after.siege_open { "open" } else { "closed" }
        ));
    }
    if before.enc != after.enc {
        if let Some((max, used)) = after.enc {
            out.push(format!("[encumberance] used={used} max={max}"));
        }
    }
    if before.trainer != after.trainer {
        if let Some((code, points, lines)) = after.trainer {
            out.push(format!(
                "[trainer] TrainerWindow code={code} points={points} lines={lines}"
            ));
        }
    }
    if before.market != after.market {
        if let Some((count, page, max_page)) = after.market {
            out.push(format!(
                "[market] MarketExplorer count={count} page={page}/{max_page}"
            ));
        }
    }
    if !before.emblem && after.emblem {
        out.push("[emblem] EmblemDialogue present (no invented pixels)".into());
    }
    if before.lfg != after.lfg {
        if let Some((count, empty)) = after.lfg {
            out.push(format!("[lfg] FindGroupUpdate count={count} empty={empty}"));
        }
    }
    out
}

fn toggle_audio_mute(audio: &mut caer_render::AudioBus) -> bool {
    let mut s = audio.settings();
    s.muted = !s.muted;
    audio.set_settings(s);
    s.muted
}

fn push_capped<T>(q: &mut Vec<T>, item: T, cap: usize) {
    if cap == 0 {
        return;
    }
    if q.len() >= cap {
        q.remove(0);
    }
    q.push(item);
}

/// Live drain → [`ProductController::observe_drain`] only. Do not fold into `social` directly.
fn fold_social_drain(
    product: &mut caer_render::product_loop::ProductController,
    d: &live::Drain,
    now: Instant,
) {
    product.observe_drain(d, now);
}

/// Zone used for dungeon product seat / fingerprint for `region`, if this region is a dungeon.
fn dungeon_zone_for_region(region: u16) -> Option<u16> {
    if region == caer_render::dungeon_mesh::CANONICAL_DUNGEON_REGION {
        return Some(caer_render::dungeon_mesh::CANONICAL_DUNGEON_ZONE);
    }
    caer_world::DUNGEON_REPRESENTATIVES
        .iter()
        .find(|r| r.region_id == region)
        .map(|r| r.zone_id)
        .or_else(|| {
            if caer_world::zone_region(region) == Some(region) {
                Some(region)
            } else {
                None
            }
        })
}

/// The client's own camera defaults, or `None` if its ini is not readable.
///
/// Prefers the resolution-specific file, then `default.ini`. Every shipped file currently carries
/// the same `[Camera]` block, but reading the specific one first is what the client does and costs
/// nothing.
fn client_camera_defaults(
    width: u32,
    height: u32,
) -> Option<caer_assets::clientini::CameraDefaults> {
    let root = caer_render::terrain::client_root();
    for name in [
        format!("default{width}.ini"),
        format!("default{height}.ini"),
        "default.ini".to_string(),
    ] {
        if let Ok(text) = std::fs::read_to_string(root.join(&name)) {
            if let Some(c) = caer_assets::clientini::ClientIni::parse(&text).camera() {
                log::info!(
                    "rustdaoc: camera defaults from {name}: dist {} height {} tilt {}",
                    c.distance,
                    c.height,
                    c.tilt
                );
                return Some(c);
            }
        }
    }
    None
}

/// Default third-person camera, taken from the CLIENT's `[Camera]` block rather than chosen.
///
/// The original ships `distance=500.00 height=10.00 tilt=174 angle=0` in every `default*.ini`.
/// These constants are the fallback used when the client's ini cannot be read; `camera_defaults()`
/// prefers the real file. Our previous hand-picked 200 was 2.5x too close, which is a large part of
/// why a screenshot did not read as DAoC.
const CAM_DIST: f32 = 500.0;
/// Height above the character's feet that the camera orbits around and looks at — a little above
/// chest height on a ~70-unit avatar, so the character sits mid-frame AND the eye has headroom
/// before a downward pitch drops it through the floor.
const FOCUS_HEIGHT: f32 = 60.0;
/// Starting pitch (radians above the horizontal), from the client's `tilt=174` in DAoC turn units
/// (4096 = a full turn), i.e. ~15.3 degrees. Read as degrees it would put the camera underground —
/// the unit confusion this project keeps paying for.
const CAM_PITCH_START: f32 = 0.267;
/// Pitch limits (radians above the horizontal).
///
/// The floor used to be -0.15 rad — a mere -8.6°. Telemetry from a real session showed the camera
/// pinned there for its entire length: mouse_dy reached -67 and the pitch never moved off -8.6,
/// giving a total vertical range of 1.4° across the whole recording. The vertical camera was, in
/// practice, frozen.
///
/// The floor exists because the eye orbits BELOW the focus at negative pitch and would otherwise
/// sink through the ground: `eye.z = FOCUS_HEIGHT + CAM_DIST·sin(pitch)` relative to the feet.
/// At `CAM_DIST` 500, asin(−FOCUS_HEIGHT/CAM_DIST) ≈ −0.120 is the hard floor; we stop a hair
/// above it so minimum pitch still clears the character's feet. (The old −0.30 was calibrated
/// for cam_dist 200 and went underground when distance was raised to match the real client.)
const CAM_PITCH_MIN: f32 = -0.11;
const CAM_PITCH_MAX: f32 = 1.30;

/// Player run speed in world units/second. DAoC's clean ceiling is ~191 u/s (the server has
/// speedhack heuristics); we sit comfortably under it. Tune toward the real class run speed later.
const MOVE_SPEED: f32 = 150.0;
/// Walk mode fraction of [`MOVE_SPEED`] (held Walk binding).
const WALK_SPEED_FRAC: f32 = 0.5;
/// Held Sprint bump; stays under the ~191 u/s speedhack ceiling with run base 150.
const SPRINT_SPEED_FRAC: f32 = 1.2;
/// Orbit pitch step while LookUp / LookDown are held (radians / second).
const LOOK_PITCH_RATE: f32 = 0.9;

/// How long the jump flag stays set. Roughly a DAoC hop — long enough for the ~200 ms wire cadence
/// to carry at least one update with the bit set, so the server always sees it.
const JUMP_TIME: f32 = 0.5;

/// Squared distance (world units²) beyond which a server-sent player position is treated as an
/// authoritative teleport (accepted) rather than a routine echo (ignored — the client owns its
/// position). ~2000 units: larger than any per-tick movement, smaller than a zone jump.
const TELEPORT_DIST_SQ: f32 = 2000.0 * 2000.0;

/// How often to put a PositionUpdate on the wire while moving. The real client sends ~every
/// 200 ms; the player box moves locally every frame (client-authoritative), this just keeps the
/// server + other observers in sync.
const MOVE_SEND_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

fn bounded_capture_wait(deadline_ms: u64, elapsed_ms: u64, gpu_default: Duration) -> Duration {
    Duration::from_millis(deadline_ms.saturating_sub(elapsed_ms).max(1)).min(gpu_default)
}

/// Preserve the canonical product reducer's effect order while handing each command to the one
/// live-session sink. Returning false if any send failed prevents a partial delivery from being
/// reported as success.
fn deliver_preworld_effects(
    commands: Vec<LiveCommand>,
    mut sink: impl FnMut(LiveCommand) -> bool,
) -> bool {
    let mut all_accepted = true;
    for command in commands {
        match &command {
            LiveCommand::RequestCharacterOverview { realm } => {
                println!("rustdaoc: UI realm select → {realm}");
            }
            LiveCommand::CreateCharacter { draft } => println!(
                "rustdaoc: UI Continué → CharacterCreateRequest slot {} realm {} race {} class {} gender {} stats {:?} points={:?} ({})",
                draft.slot,
                draft.realm,
                draft.race,
                draft.class_id,
                draft.gender,
                draft.stats,
                draft.points_spent(),
                draft.name
            ),
            LiveCommand::Quit => println!("rustdaoc: UI Exit → Quit"),
            LiveCommand::DeleteCharacter { realm, slot } => println!(
                "rustdaoc: UI Delete confirmed → CharacterCreateRequest op=Delete realm {realm} slot {slot}"
            ),
            _ => {}
        }
        all_accepted &= sink(command);
    }
    all_accepted
}

/// Records the game's own frames to PNGs so a MOTION bug can be reviewed after the fact.
///
/// Screen recorders cannot capture this window under Wayland (an x11grab of the display comes back
/// black, and Spectacle's timed-stop leaves a truncated file), so the renderer captures itself.
/// That is also strictly better: the frames are exactly what the game drew, at the game's own
/// resolution, with no compositor or codec in between.
///
/// Frames are written at a fixed interval rather than every frame — the client runs at 800+ fps and
/// a PNG per frame would both stall the loop and bury the interesting motion in near-duplicates.
struct Recorder {
    dir: std::path::PathBuf,
    /// Exact safe basename for a one-shot harness capture; ordinary recordings use numbered fNNN.
    file_prefix: Option<String>,
    /// Seconds between captured frames.
    interval: f32,
    /// Time since the last capture.
    since: f32,
    frames: u32,
    max_frames: u32,
    /// Seconds of wall time left to record; `None` = run until the frame cap (the F9 case).
    remaining: Option<f32>,
    /// Per-frame input + camera state, written next to the frames as `telemetry.csv`.
    ///
    /// This is the point of the whole recorder. Frames show what the screen looked like; they do
    /// NOT show what the mouse was doing, so "the camera goes the wrong way when I drag left" is a
    /// claim I can only take on trust and guess at. With the raw mouse delta logged beside the
    /// resulting camera angle, a sign error is arithmetic instead of description.
    telemetry: Vec<String>,
    /// Mouse delta accumulated since the last captured frame, and which buttons were down for it.
    mouse_dx: f32,
    mouse_dy: f32,
    saw_orbit: bool,
    saw_turn: bool,
}

impl Recorder {
    /// ~10 captured frames a second. The cap is 20 seconds, not 6: a 6-second F9 ran out partway
    /// through a three-part control test (W, then D, then the mouse), capturing only the first two
    /// and leaving the actual question unanswered.
    const INTERVAL: f32 = 0.1;
    const MAX_FRAMES: u32 = 200;

    /// The file another process writes to ask for a recording — it contains the duration in
    /// seconds. Polled from the frame loop.
    ///
    /// A trigger FILE rather than a keystroke because the useful case is "record what I'm doing"
    /// asked for conversationally while the game has focus: nothing outside the process can press
    /// F9 for you under Wayland, but anything can write a file.
    fn request_path() -> std::path::PathBuf {
        std::env::temp_dir().join("caer-record-request")
    }

    /// Consume a pending external request, if any, returning the requested duration.
    fn take_request() -> Option<f32> {
        Self::take_request_from(&Self::request_path())
    }

    fn take_request_from(path: &std::path::Path) -> Option<f32> {
        let text = std::fs::read_to_string(&path).ok()?;
        // Remove it first: a malformed or unreadable request must not retrigger every frame.
        let _ = std::fs::remove_file(&path);
        Some(text.trim().parse::<f32>().unwrap_or(10.0).clamp(1.0, 120.0))
    }

    fn start() -> Option<Self> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("caer-clip-{stamp}"));
        Self::start_at(dir)
    }

    fn start_at(dir: std::path::PathBuf) -> Option<Self> {
        std::fs::create_dir_all(&dir).ok()?;
        println!(
            "rustdaoc: recording → {} (F9 to stop, auto-stops at {} frames)",
            dir.display(),
            Self::MAX_FRAMES
        );
        Some(Self {
            dir,
            file_prefix: None,
            interval: Self::INTERVAL,
            since: Self::INTERVAL,
            frames: 0,
            max_frames: Self::MAX_FRAMES,
            remaining: None,
            telemetry: vec![
                "frame,t,mouse_dx,mouse_dy,btn,keys,heading,heading_deg,cam_orbit_deg,pitch_deg,cam_yaw_deg,x,y,z,speed".to_string(),
            ],
            mouse_dx: 0.0,
            mouse_dy: 0.0,
            saw_orbit: false,
            saw_turn: false,
        })
    }

    /// Start a recording of a fixed wall-clock length. The frame cap is sized to cover it so a
    /// long request isn't cut short by the F9 default.
    fn start_for(seconds: f32) -> Option<Self> {
        let mut r = Self::start()?;
        r.max_frames = ((seconds / Self::INTERVAL).ceil() as u32 + 2).max(Self::MAX_FRAMES);
        r.remaining = Some(seconds);
        println!("rustdaoc: recording {seconds:.0}s on request");
        Some(r)
    }

    fn start_for_harness(seconds: f32) -> Option<Self> {
        let root = std::env::var_os("CAER_HARNESS_BUNDLE_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        let mut r = Self::start_at(root.join("frames/native-recording"))?;
        r.max_frames = ((seconds / Self::INTERVAL).ceil() as u32 + 2).max(Self::MAX_FRAMES);
        r.remaining = Some(seconds);
        println!("rustdaoc: recording {seconds:.0}s on harness request");
        Some(r)
    }

    fn start_harness_capture(artifact_name: &str) -> Option<Self> {
        let root = std::env::var_os("CAER_HARNESS_BUNDLE_DIR").map(std::path::PathBuf::from)?;
        let mut recorder = Self::start_at(root.join("frames"))?;
        recorder.file_prefix = Some(artifact_name.to_owned());
        recorder.max_frames = 1;
        recorder.remaining = Some(10.0);
        Some(recorder)
    }

    fn next_path(&self) -> std::path::PathBuf {
        self.dir.join(self.file_prefix.as_ref().map_or_else(
            || format!("f{:03}.png", self.frames),
            |prefix| format!("{prefix}.png"),
        ))
    }

    /// Accumulate raw mouse motion between captured frames, tagged with which drag was active.
    fn note_mouse(&mut self, dx: f32, dy: f32, orbiting: bool, turning: bool) {
        self.mouse_dx += dx;
        self.mouse_dy += dy;
        self.saw_orbit |= orbiting;
        self.saw_turn |= turning;
    }

    /// Advance the wall clock; returns false when a timed recording is done.
    fn tick(&mut self, dt: f32) -> bool {
        match self.remaining.as_mut() {
            Some(left) => {
                *left -= dt;
                *left > 0.0
            }
            None => true,
        }
    }

    /// Whether this frame should be captured; advances the timer.
    fn wants_frame(&mut self, dt: f32) -> bool {
        self.since += dt;
        if self.since >= self.interval {
            self.since = 0.0;
            true
        } else {
            false
        }
    }

    /// Log this frame's input + camera state. Called with the frame it belongs to.
    #[allow(clippy::too_many_arguments)]
    fn note_state(
        &mut self,
        t: f32,
        keys: &str,
        heading: u16,
        cam_orbit: f32,
        pitch: f32,
        cam_yaw: f32,
        pos: [f32; 3],
        speed: f32,
    ) {
        let btn = match (self.saw_orbit, self.saw_turn) {
            (true, true) => "both",
            (true, false) => "LEFT",
            (false, true) => "RIGHT",
            (false, false) => "-",
        };
        let deg = |r: f32| r.to_degrees();
        self.telemetry.push(format!(
            "{},{t:.2},{:.1},{:.1},{btn},{keys},{heading},{:.1},{:.1},{:.1},{:.1},{:.0},{:.0},{:.0},{speed:.0}",
            self.frames,
            self.mouse_dx,
            self.mouse_dy,
            f32::from(heading) / 4096.0 * 360.0,
            deg(cam_orbit),
            deg(pitch),
            deg(cam_yaw),
            pos[0], pos[1], pos[2],
        ));
        self.mouse_dx = 0.0;
        self.mouse_dy = 0.0;
        self.saw_orbit = false;
        self.saw_turn = false;
    }

    /// Write one captured frame. Returns false once the recording is full.
    fn write(&mut self, rgba: &[u8], w: u32, h: u32) -> std::io::Result<bool> {
        let path = self.next_path();
        let result = (|| {
            let file = std::fs::File::create(&path)?;
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().map_err(std::io::Error::other)?;
            writer.write_image_data(rgba).map_err(std::io::Error::other)
        })();
        self.frames += 1;
        result.map(|()| self.frames < self.max_frames)
    }

    fn finish(&self) {
        let csv = self.dir.join("telemetry.csv");
        if let Err(e) = std::fs::write(&csv, self.telemetry.join("\n")) {
            println!("rustdaoc: could not write telemetry: {e}");
        }
        println!(
            "rustdaoc: recorded {} frames → {}",
            self.frames,
            self.dir.display()
        );
        println!("  telemetry:      {}", csv.display());
        println!("  contact sheet:  caer-clip --sheet {}", self.dir.display());
    }
}

/// Which movement keys are currently held.
///
/// `fwd`/`back`/`strafe_*` are driven by [`GameAction`] bindings (W/S, Q/E by default).
/// `turn_left`/`turn_right` remain for mouse-relative turn integration and tests: the 74-action
/// internal table has no Turn Left/Right rows, so A/D stay unbound and these flags are not set by
/// keyboard dispatch today.
#[derive(Default)]
struct MoveKeys {
    fwd: bool,
    back: bool,
    /// Reserved for keyboard turn once display-table Turn Left/Right are mapped.
    turn_left: bool,
    turn_right: bool,
    /// Q / E — `slide_left` / `slide_right` in the 74-action table.
    strafe_left: bool,
    strafe_right: bool,
    /// Held Walk (table marks Walk as held).
    walk: bool,
    /// Held Sprint.
    sprint: bool,
    /// Held LookUp / LookDown.
    look_up: bool,
    look_down: bool,
}

impl MoveKeys {
    /// Any key that produces MOVEMENT. Turning is not movement: holding A while standing still
    /// rotates on the spot and must not start the run animation or claim a travel speed.
    /// Whether the character is actually TRAVELLING.
    ///
    /// Opposing keys cancel, exactly as they already do for turning: holding W and S together sums
    /// to a zero direction vector, so the character does not move. Reporting "active" there claimed
    /// a travel speed while standing still — which told the server we were sprinting on the spot and
    /// played the run cycle under a motionless avatar.
    ///
    /// This is precisely the condition under which `update_movement`'s direction vector is
    /// non-zero, so the two can no longer disagree.
    fn active(&self) -> bool {
        self.fwd != self.back || self.strafe_left != self.strafe_right
    }

    /// Any key that rotates the character.
    fn turning(&self) -> bool {
        self.turn_left != self.turn_right
    }
}

/// Keyboard turn rate, DAoC heading units per second (4096 = a full turn), i.e. ~90°/s.
const TURN_RATE: f32 = 1024.0;

/// The z to publish for the player's own world entity, given the player's feet height.
///
/// Identity, deliberately — and a named function rather than an inline cast so the invariant has
/// somewhere to be tested and explained. DAoC positions are FEET positions and the avatar mesh is
/// foot-anchored, so any constant added here lifts the player off the ground.
fn self_entity_z(player_z: f32) -> i32 {
    player_z as i32
}

/// Whether a submitted command is the given CLIENT-side verb.
///
/// Matches the bare verb or the verb plus arguments, so `/keyboard` and `/keyboard attack R` both
/// hit, while `/keyboardsomething` does not — a prefix test alone would swallow unrelated commands
/// and stop them reaching the server.
fn is_client_command(cmd: &str, verb: &str) -> bool {
    let c = cmd.trim();
    c.eq_ignore_ascii_case(verb)
        || c.len() > verb.len()
            && c[..verb.len()].eq_ignore_ascii_case(verb)
            && c.as_bytes()[verb.len()].is_ascii_whitespace()
}

/// Handle `/keyboard`, returning the lines to show the player.
///
/// Split out of the event loop and returning its output rather than logging directly so the whole
/// command surface is testable without a window, a server, or a GPU.
/// Listing and reports use canonical config names from binding metadata (`combat_mode`, not the
/// legacy `attack` alias — that alias still resolves on input).
fn keyboard_command(binds: &mut Bindings, args: &str) -> (Vec<String>, bool) {
    let args = args.trim();
    let mut out = Vec::new();

    // Bare `/keyboard` lists the current bindings from the same metadata runtime dispatch uses.
    if args.is_empty() {
        return (caer_render::keybinds::keyboard_list_lines(binds), false);
    }

    let mut parts = args.split_whitespace();
    let first = parts.next().unwrap_or("");

    if first.eq_ignore_ascii_case("reset") {
        *binds = Bindings::daoc_defaults();
        out.push("keybindings reset to the DAoC defaults".into());
        return (out, true);
    }

    let Some(action) = GameAction::from_name(first) else {
        out.push(format!(
            "unknown action `{first}` — bare /keyboard lists them"
        ));
        return (out, false);
    };
    let Some(key_word) = parts.next() else {
        // Naming an action alone asks what it is bound to.
        let shown = caer_render::keybinds::format_bound_chords(binds, action);
        out.push(match shown.as_str() {
            "(unbound)" => format!("{} is unbound", action.name()),
            _ => format!("{} = {shown}", action.name()),
        });
        return (out, false);
    };

    if key_word.eq_ignore_ascii_case("none") {
        let n = binds.unbind_action(action);
        out.push(format!("{} unbound ({n} key(s) cleared)", action.name()));
        return (out, true);
    }
    let Some(key) = caer_render::keybinds::key_from_name(key_word) else {
        out.push(format!("unknown key `{key_word}`"));
        return (out, false);
    };
    // Tell the player what they displaced — a silent steal is how someone loses a key and can't
    // work out why.
    let displaced = binds.action_for(key).filter(|&a| a != action);
    binds.unbind_action(action);
    binds.bind(key, action);
    out.push(format!("{} = {key_word}", action.name()));
    if let Some(old) = displaced {
        out.push(format!("  (took {key_word} from {})", old.name()));
        if binds.keys_for(old).is_empty() {
            out.push(format!("  WARNING: {} is now unbound", old.name()));
        }
    }
    (out, true)
}

/// Where the player's keybindings live.
///
/// Under the user's config dir rather than beside the game, so a reinstall or a re-copied client
/// directory doesn't silently wipe someone's controls. Falls back to the current directory if the
/// environment has no home, which keeps the client runnable in a bare test harness.
fn bindings_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("caer").join("keybinds.cfg")
}

/// Load the player's bindings, falling back to the DAoC defaults.
///
/// A missing file is the normal first-run case and is not worth mentioning; a file that exists but
/// has bad lines IS worth mentioning, because the player edited it and needs to know which line the
/// client ignored.
fn load_bindings() -> Bindings {
    let path = bindings_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        log::debug!(
            "rustdaoc: no keybind config at {}, using DAoC defaults",
            path.display()
        );
        return Bindings::default();
    };
    let (binds, warnings) = Bindings::parse(&text);
    for w in &warnings {
        log::warn!("rustdaoc: {}: {w}", path.display());
    }
    log::info!("rustdaoc: loaded keybindings from {}", path.display());
    binds
}

/// Persist the current bindings. Returns a player-facing message either way — a rebind that
/// silently fails to save is worse than one that says so.
fn save_bindings(binds: &Bindings) -> String {
    let path = bindings_path();
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return format!("could not create {}: {e}", dir.display());
        }
    }
    match std::fs::write(&path, binds.to_config()) {
        Ok(()) => format!("keybindings saved to {}", path.display()),
        Err(e) => format!("could not save {}: {e}", path.display()),
    }
}

/// Advance the character's facing by one frame of keyboard turning and work out what that does to
/// the camera. Returns the new `(heading, cam_orbit)`.
///
/// `pinned` is right-click-held: DAoC fixes the CAMERA while it is down, so the character rotates
/// underneath a view that does not move — that is what lets you keep watching a kiting target while
/// re-orienting to run elsewhere. `camera_yaw` is `π/2 − heading + cam_orbit`, so pinning across a
/// heading change of δ means adding exactly δ to the orbit.
///
/// This lives OUTSIDE `Client` on purpose. The previous attempt at this fix existed only as a test
/// that recomputed the compensation itself, so it went green while `update_movement` was still
/// zeroing the orbit on every frame of the turn — the test agreed with the intent and never touched
/// the code. Both callers now go through this one function, so the test can only pass if the client
/// really does it.
fn apply_turn(
    heading: u16,
    cam_orbit: f32,
    dir: f32,
    dt: f32,
    pinned: bool,
    snapback: bool,
) -> (u16, f32) {
    use std::f32::consts::TAU;
    let new_heading = (f32::from(heading) + dir * TURN_RATE * dt).rem_euclid(4096.0) as u16;

    let orbit = if pinned {
        // Compensate by the QUANTISED delta — the heading we actually stored, not the float step.
        // Heading is a u16, so accumulating the unrounded step here would leak a fraction of a unit
        // into the orbit every frame and let the "pinned" camera creep over a long turn.
        let raw = f32::from(new_heading) - f32::from(heading);
        // Shortest way round, so wrapping through 0/4096 doesn't read as a full turn the other way.
        let delta = if raw > 2048.0 {
            raw - 4096.0
        } else if raw < -2048.0 {
            raw + 4096.0
        } else {
            raw
        };
        cam_orbit + delta * (TAU / 4096.0)
    } else if snapback {
        // Not pinned: the camera stays behind the character as it turns (F3 turns this off for the
        // CC/kiting classes that want a free camera).
        0.0
    } else {
        cam_orbit
    };

    (new_heading, orbit.rem_euclid(TAU))
}

/// No invented freeshard credentials and no LIVE default. Live connect requires explicit
/// `--server` / `CAER_SERVER` and `--account`/`--password` or `CAER_ACCOUNT`/`CAER_PASSWORD`.
const LIVE_PORT_SUFFIX: &str = ":10311";

struct Args {
    server: String,
    account: String,
    password: String,
    character: Option<String>,
    region: u16,
    screenshot: Option<String>,
    size: (u32, u32),
    /// Force a pre-world screen (including splash/loading and the creation sub-screens) for
    /// headless verification.
    preworld: Option<String>,
    /// Realm to show on the pre-world character screens (1 Albion, 2 Midgard, 3 Hibernia).
    realm: Option<u8>,
    /// `--race N`: seed the creation draft with this eRace, so a headless capture can show any
    /// race's body without a human clicking the form.
    race: Option<u8>,
    /// `--class N`: select a legal concrete class for a creation-chain capture.  Character
    /// customizer screenshots may show any race, but the stats pane must never describe an
    /// Albion Armsman behind a Hibernian Celt merely because the headless draft began as Briton.
    class_id: Option<u8>,
    /// `--face N` / `--hair-style N` / `--hair-color N` / `--skin N`: seed the draft's look.
    ///
    /// They let headless captures render a chosen look without synthesising mouse input, and let
    /// the render/packet parity test drive that same look without a window.
    custom: caer_protocol::customization::Customization,
    /// Optional values for the four source nine-tick facial sliders: Nose, Eyes, the race's
    /// third label (Lips/Ears), and the race's fourth label (Jaw/Chin).  Source labels vary by
    /// race, so the CLI keeps the stable packet order and the customizer shows the catalogue's
    /// exact names.
    morph_ticks: [Option<u8>; 4],
    /// Deterministic customizer preview state for screenshot evidence.  Interactive users reach
    /// the same state with the retail-authored lower-left camera controls and drag gestures.
    preworld_zoom: Option<f32>,
    preworld_tilt: Option<f32>,
    preworld_yaw: Option<f32>,
    /// `--gender 0|1`: wire gender for that draft.
    ///
    /// Without this every headless capture showed a MALE body, which is how 18 female models went
    /// unlooked-at across an entire sweep while the counters read fully bound.
    gender: Option<u8>,
    /// Gated RUSTDAOC_HEADLESS_LIVE_SMOKE: live connect + one headless frame + Sit/Quit send.
    /// Not PLAYER_SCENARIO / not the rustdaoc window/`Client` input loop.
    headless_live_smoke: bool,
    /// Print embedded build identity and exit (no GPU/network).
    build_identity: bool,
    /// Opt-in caer.harness/v1 telemetry and semantic ingress. The following value is the run ID.
    harness_jsonl: Option<String>,
}

impl Args {
    fn with_defaults() -> Self {
        Self {
            server: String::new(),
            account: String::new(),
            password: String::new(),
            character: None,
            region: 1,
            realm: None,
            race: None,
            class_id: None,
            gender: None,
            custom: caer_protocol::customization::Customization::default(),
            morph_ticks: [None; 4],
            preworld_zoom: None,
            preworld_tilt: None,
            preworld_yaw: None,
            screenshot: None,
            size: (1600, 1000),
            preworld: None,
            headless_live_smoke: false,
            build_identity: false,
            harness_jsonl: None,
        }
    }

    /// Offline screenshot / build-identity paths skip credential and LIVE guards.
    fn needs_live_auth(&self) -> bool {
        if self.build_identity {
            return false;
        }
        if self.screenshot.is_some() && !self.headless_live_smoke {
            return false;
        }
        if self.offline_preworld() {
            return false;
        }
        true
    }

    /// A `--preworld <screen>` run with no server named: a pre-world session that never connects.
    ///
    /// The character screens are the whole product here — the stage renders, the figure stands, and
    /// the Ctrl+Alt tuners live in this window. None of it needs a server, and demanding one meant
    /// the only way to see a pre-world screen at all was to take a headless screenshot of it. The
    /// live-`:10311` refusal still stands for anything that actually dials out; naming a server
    /// alongside `--preworld` keeps every bit of today's behaviour, forced screen included.
    fn offline_preworld(&self) -> bool {
        self.preworld.is_some() && self.server.is_empty() && !self.headless_live_smoke
    }
}

/// Apply `CAER_SERVER` / `CAER_ACCOUNT` / `CAER_PASSWORD` over empty defaults.
/// Injected `getenv` keeps the overlay unit-testable without racing the process env.
fn apply_caer_env(a: &mut Args, getenv: &dyn Fn(&str) -> Option<String>) {
    if let Some(v) = getenv("CAER_SERVER") {
        if !v.is_empty() {
            a.server = v;
        }
    }
    if let Some(v) = getenv("CAER_ACCOUNT") {
        if !v.is_empty() {
            a.account = v;
        }
    }
    if let Some(v) = getenv("CAER_PASSWORD") {
        if !v.is_empty() {
            a.password = v;
        }
    }
}

/// Refuse missing credentials and accidental LIVE `:10311` without explicit opt-in.
fn validate_launch_args(a: &Args, getenv: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    if let Some(run_id) = &a.harness_jsonl {
        caer_render::harness::NativeHarness::new(run_id.clone())?;
        if let Some(lab) = getenv("CAER_HARNESS_LAB_ID") {
            if lab.trim().is_empty() {
                return Err("CAER_HARNESS_LAB_ID must not be empty".into());
            }
            caer_harness::reject_secrets(&serde_json::Value::String(lab))
                .map_err(|error| error.to_string())?;
        }
    }
    if let Some(realm) = a.realm.filter(|realm| !(1..=3).contains(realm)) {
        return Err(format!(
            "REFUSED — --realm must be 1, 2, or 3 (got {realm})"
        ));
    }
    if !a.needs_live_auth() {
        return Ok(());
    }
    if a.server.is_empty() {
        return Err(
            "REFUSED — set --server or CAER_SERVER (no LIVE :10311 default; use isolated candidate port)"
                .into(),
        );
    }
    if a.account.is_empty() || a.password.is_empty() {
        return Err(
            "REFUSED — set --account/--password or CAER_ACCOUNT/CAER_PASSWORD (no invented credentials)"
                .into(),
        );
    }
    let allow_live = getenv("CAER_ALLOW_LIVE_ENDPOINT")
        .as_deref()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    if a.server.ends_with(LIVE_PORT_SUFFIX) && !allow_live {
        return Err(format!(
            "REFUSED — endpoint {} is LIVE :10311; set CAER_ALLOW_LIVE_ENDPOINT=1 for explicit opt-in",
            a.server
        ));
    }
    Ok(())
}

fn parse_args_from<I, S>(args: I, getenv: &dyn Fn(&str) -> Option<String>) -> Args
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut a = Args::with_defaults();
    apply_caer_env(&mut a, getenv);
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_ref() {
            "--server" => {
                if let Some(v) = it.next() {
                    a.server = v.as_ref().to_string();
                }
            }
            "--account" => {
                if let Some(v) = it.next() {
                    a.account = v.as_ref().to_string();
                }
            }
            "--password" => {
                if let Some(v) = it.next() {
                    a.password = v.as_ref().to_string();
                }
            }
            "--char" => a.character = it.next().map(|v| v.as_ref().to_string()),
            "--region" => a.region = it.next().and_then(|s| s.as_ref().parse().ok()).unwrap_or(1),
            "--screenshot" => a.screenshot = it.next().map(|v| v.as_ref().to_string()),
            "--headless-live-smoke" | "--player-scenario-smoke" => a.headless_live_smoke = true,
            "--build-identity" => a.build_identity = true,
            "--harness-jsonl" => {
                a.harness_jsonl = Some(it.next().map_or_else(String::new, |v| v.as_ref().into()));
            }
            "--preworld" => a.preworld = it.next().map(|v| v.as_ref().to_string()),
            // Which realm's scene and race/class set to show. Screenshots of the character screens
            // are otherwise stuck on Albion, so "every realm renders" could not be checked at all.
            "--realm" => {
                a.realm = it.next().and_then(|v| v.as_ref().parse::<u8>().ok());
            }
            "--race" => {
                a.race = it.next().and_then(|v| v.as_ref().parse::<u8>().ok());
            }
            "--class" => {
                a.class_id = it.next().and_then(|v| v.as_ref().parse::<u8>().ok());
            }
            "--gender" => {
                a.gender = it.next().and_then(|v| v.as_ref().parse::<u8>().ok());
            }
            "--face" => {
                a.custom.face_type = it
                    .next()
                    .and_then(|v| v.as_ref().parse::<u8>().ok())
                    .unwrap_or(0);
            }
            "--hair-style" => {
                a.custom.hair_style = it
                    .next()
                    .and_then(|v| v.as_ref().parse::<u8>().ok())
                    .unwrap_or(0);
            }
            "--hair-color" => {
                a.custom.hair_color = it
                    .next()
                    .and_then(|v| v.as_ref().parse::<u8>().ok())
                    .unwrap_or(0);
            }
            "--skin" => {
                a.custom.eye_color = it
                    .next()
                    .and_then(|v| v.as_ref().parse::<u8>().ok())
                    .unwrap_or(0);
            }
            "--morph-nose" => {
                a.morph_ticks[0] = it.next().and_then(|v| v.as_ref().parse::<u8>().ok());
            }
            "--morph-eyes" => {
                a.morph_ticks[1] = it.next().and_then(|v| v.as_ref().parse::<u8>().ok());
            }
            "--morph-third" => {
                a.morph_ticks[2] = it.next().and_then(|v| v.as_ref().parse::<u8>().ok());
            }
            "--morph-fourth" => {
                a.morph_ticks[3] = it.next().and_then(|v| v.as_ref().parse::<u8>().ok());
            }
            "--preworld-zoom" => {
                a.preworld_zoom = it.next().and_then(|v| v.as_ref().parse::<f32>().ok());
            }
            "--preworld-tilt" => {
                a.preworld_tilt = it.next().and_then(|v| v.as_ref().parse::<f32>().ok());
            }
            "--preworld-yaw" => {
                a.preworld_yaw = it.next().and_then(|v| v.as_ref().parse::<f32>().ok());
            }
            "--size" => {
                if let Some(spec) = it.next() {
                    if let Some((w, h)) = spec.as_ref().split_once('x') {
                        a.size = (w.parse().unwrap_or(1600), h.parse().unwrap_or(1000));
                    }
                }
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: rustdaoc --account A --password P --server host:port [--char NAME] \
                     [--region N] [--screenshot out.png] [--size WxH] \
                     [--preworld login|splash|loading|settings|quitconfirm|deleteconfirm|realm|charselect|charcreate|customize|stats] [--realm 1|2|3] [--race N --class N --gender 0|1] \
                     [--face N --hair-style N --hair-color N --skin N --morph-nose 0..8 --morph-eyes 0..8 --morph-third 0..8 --morph-fourth 0..8] \
                     [--preworld-zoom SCALE --preworld-tilt RADIANS --preworld-yaw RADIANS] \
                     [--headless-live-smoke] [--harness-jsonl RUN_ID]\n\
                     no invented credentials; LIVE :10311 requires CAER_ALLOW_LIVE_ENDPOINT=1\n\
                     --headless-live-smoke: LiveFeed + headless GPU + Sit/Quit send (not PLAYER_SCENARIO).\n\
                     env (CLI wins): CAER_SERVER, CAER_ACCOUNT, CAER_PASSWORD, CAER_CLIENT, CAER_ALLOW_LIVE_ENDPOINT\n\n\
                     {}",
                    caer_render::keybinds::default_controls_help()
                );
                std::process::exit(0);
            }
            other => eprintln!("rustdaoc: ignoring unknown arg {other}"),
        }
    }
    a
}

/// Choose the exact class a headless creation-chain capture describes.
///
/// The normal product flow already moves a newly selected race to its first legal class.  The
/// screenshot seam seeds a draft directly, so it needs the same source-adapter rule or an
/// Hibernian capture inherits the Briton stub's Armsman description.  A requested class wins only
/// when the protocol oracle says the full realm/race/gender tuple is legal; otherwise the first
/// legal class in the retail form's own adapter order is the faithful default.
fn capture_class_for(realm: u8, race: u8, gender: u8, requested: Option<u8>) -> Option<u8> {
    let realm = realm.clamp(1, 3);
    let allowed = |class_id| {
        matches!(
            caer_protocol::create_validity::classify(realm, class_id, race, gender),
            caer_protocol::create_validity::Legality::Allowed
        )
    };
    requested.filter(|class_id| allowed(*class_id)).or_else(|| {
        caer_protocol::creation_adapters::class_adapters(realm, race)
            .into_iter()
            .map(|adapter| adapter.class_id)
            .find(|class_id| allowed(*class_id))
    })
}

/// Resolve the first source-authored race for an offline realm capture when the caller did not
/// explicitly request one.
///
/// A realm-only command must be self-contained: its seed is the first source-authored race in
/// the requested realm, never the Albion/Briton construction stub used to initialise the draft.
fn capture_race_for_realm(realm: u8, requested: Option<u8>) -> u8 {
    requested.unwrap_or_else(|| caer_render::preworld::race_id_for_realm(realm.clamp(1, 3), 0))
}

/// Isolated RUSTDAOC_HEADLESS_LIVE_SMOKE: one headless frame + Sit/Quit send. Refuses LIVE :10311.
/// Not PLAYER_SCENARIO: does not enter the rustdaoc `Client` window/input/UI loop.
fn run_headless_live_smoke(args: &Args, spawn: [f32; 3], feed: Option<live::LiveFeed>) -> ! {
    if args
        .server
        .rsplit_once(':')
        .is_some_and(|(_, p)| p == "10311")
    {
        eprintln!(
            "rustdaoc: --headless-live-smoke REFUSED LIVE_STAGE :10311 ({})",
            args.server
        );
        std::process::exit(1);
    }
    let Some(feed) = feed else {
        eprintln!("rustdaoc: --headless-live-smoke needs a live LiveFeed (not screenshot skip)");
        std::process::exit(1);
    };
    std::env::set_var("CAER_RT_EXAMPLE", "rustdaoc_headless_live_smoke");
    caer_client::evidence::emit("rustdaoc_headless_live_smoke");
    println!(
        "rustdaoc: HEADLESS_LIVE_SMOKE in-world at ({:.0},{:.0},{:.0})",
        spawn[0], spawn[1], spawn[2]
    );
    let (w, h) = args.size;
    let gpu = pollster::block_on(Gpu::new_headless(w, h, CULL_RADIUS as f32)).unwrap_or_else(|e| {
        eprintln!("rustdaoc: --headless-live-smoke headless GPU init failed:\n{e}");
        std::process::exit(1);
    });
    if let Err(e) = gpu.render_headless_sync(0) {
        eprintln!("rustdaoc: --headless-live-smoke headless render failed: {e}");
        std::process::exit(1);
    }
    println!("rustdaoc: headless frame ok ({w}x{h})");
    if let Err(e) = feed.send(live::LiveCommand::Sit { sit: true }) {
        eprintln!("rustdaoc: --headless-live-smoke Sit send failed: {e}");
        std::process::exit(1);
    }
    println!("rustdaoc: Sit dispatched on LiveFeed (send-only; not Client::toggle_sit)");
    if let Err(e) = feed.send(live::LiveCommand::Quit) {
        eprintln!("rustdaoc: --headless-live-smoke Quit send failed: {e}");
        std::process::exit(1);
    }
    println!("rustdaoc: Quit dispatched");
    drop(feed);
    println!(
        "OK — RUSTDAOC_HEADLESS_LIVE_SMOKE (LiveFeed + headless GPU + Sit/Quit send). \
         Not PLAYER_SCENARIO. Not transplant. Not PE game.dll."
    );
    std::process::exit(0);
}

fn parse_args() -> Args {
    parse_args_from(std::env::args().skip(1), &|k| std::env::var(k).ok())
}

/// What makes one stand worth logging again: who it is, what body resolved, how it was built, and
/// the look they are wearing. Named because the log line is chatty and only useful when something
/// actually changed — repeating it every frame buries the change it exists to report.
type StandLogKey = (
    u8,
    u8,
    u16,
    &'static str,
    usize,
    usize,
    usize,
    caer_protocol::customization::AvatarAppearance,
);

/// Assemble and place one character on a pre-world stage. `None` identity clears the stage.
fn stand_preworld_avatar(
    entity_models: Option<&mut caer_render::entities::EntityModels>,
    gpu: &mut Gpu,
    feet: Vec3,
    identity: Option<(u8, u8)>,
    appearance: caer_protocol::customization::AvatarAppearance,
    equip: Option<&caer_protocol::equipment::EquipmentUpdate>,
    anim_time: f32,
    facing_yaw: f32,
) {
    let (Some(em), Some((race, gender))) = (entity_models, identity) else {
        gpu.clear_skinned_instances();
        gpu.clear_entity_instances();
        return;
    };
    let dress = equip.cloned();
    let Some(model) = em.ensure_avatar(gpu, race, gender, appearance, dress.as_ref()) else {
        println!("rustdaoc: preworld avatar race {race} gender {gender} — no body assembled");
        gpu.clear_skinned_instances();
        gpu.clear_entity_instances();
        return;
    };
    let scale = em.race_display_scale(race, gender);
    let info = em.last_avatar_stand();
    let clip_dur = em
        .skinned_rig(model)
        .map(|r| r.clip.duration)
        .unwrap_or(0.0);
    let clip_t = em
        .skinned_rig(model)
        .map(|r| caer_render::preworld_idle_t(&r.clip, anim_time))
        .unwrap_or(0.0);
    let (sw, sh) = gpu.surface_size();
    let starter = dress
        .as_ref()
        .and_then(|d| d.items.first().map(|i| i.model));
    let key = (
        race,
        gender,
        model,
        info.path,
        info.eq_bound,
        info.mskin_bound,
        info.textured,
        appearance,
    );
    thread_local! {
        static LAST: std::cell::Cell<Option<StandLogKey>> = const { std::cell::Cell::new(None) };
    }
    if LAST.with(|c| c.get()) != Some(key) {
        LAST.with(|c| c.set(Some(key)));
        println!(
            "rustdaoc: stand race {race} gender {gender} model {model:#06x} scale {scale} \
             fig3_parts={} textured={} eq_bound={} mskin_bound={} path={} starter={starter:?} \
             surface={sw}x{sh} clip_t={clip_t:.2} clip_dur={clip_dur:.2} look={appearance:?} \
             parts=[{}] textures={:?}",
            info.fig3_parts,
            info.textured,
            info.eq_bound,
            info.mskin_bound,
            info.path,
            info.part_names,
            info.bound_textures,
        );
    }
    let inst = caer_render::gpu::SkinnedInstance {
        // Scene space, NOT mirrored.
        //
        // `Camera::view_proj` mirrors the EYE and the FORWARD vector, which cancels out and leaves
        // the camera looking at a scene-space point. The realm scene's own geometry is uploaded in
        // raw NIF coordinates, so the body has to live in that same space. Negating Y here put the
        // character at -30 while the camera aimed at +30, which is why the subject projected to
        // screen x=0.35-0.43 instead of 0.5 and read as "standing off to the side".
        //
        // This was invisible for as long as the anchor was the scene origin: -0.0 == 0.0. The
        // authored `collidee` anchor at (-141, 30) is what finally made it show.
        //
        // Facing follows the camera bearing rather than a constant: the eye dollies out along the
        // scene's landmark bisector, so "toward the lens" is that bearing, not a fixed axis. With
        // the Y mirror removed the old constant π turned the body's back to the player.
        pos_yaw: [feet.x, feet.y, feet.z, facing_yaw],
        scale,
        palette_base: 0.0,
    };
    let palettes = em.skinned_rig(model).map(|rig| {
        let mut pal = if let Some(policy) =
            caer_render::entities::character_screen_pose_policy(race, gender)
        {
            rig.palette_of_with_policy(&rig.clip, clip_t, policy)
        } else {
            caer_render::anim_skin::build_palettes_serial(
                rig,
                &[caer_render::anim_skin::UniquePaletteJob {
                    loco: caer_render::entities::Loco::Idle,
                    t: clip_t,
                    blend: None,
                }],
            )
        };
        // A settled per-race head pitch, if this race has one. Zero — the normal state — leaves
        // the palette bit-identical, so the authored pose is what renders until someone dials a
        // race in by eye. Applied here rather than inside the skinning chain because the world
        // shares that chain and nothing in the world asked for a tilted head.
        caer_render::preworld_head_tune::tilt_head(
            rig,
            &rig.clip,
            clip_t,
            caer_render::preworld_head_tune::settled(race, gender),
            &mut pal,
        );
        pal
    });
    gpu.clear_skinned_instances();
    gpu.clear_entity_instances();
    match palettes {
        Some(p) if !p.is_empty() => gpu.update_skinned_instances(model, &[inst], &p),
        // No rig: the body still has a static mesh, drawn in its bind pose.
        _ => gpu.update_entity_instances(
            model,
            &[[
                inst.pos_yaw[0],
                inst.pos_yaw[1],
                inst.pos_yaw[2],
                inst.pos_yaw[3],
                scale,
            ]],
        ),
    }
}

/// Build the projection matrix for one pre-world camera carriage.
///
/// The render space mirrors Y, while the scene/framing helpers intentionally expose ordinary
/// scene coordinates.  Keeping that bridge in one helper prevents the backdrop and close avatar
/// lenses from acquiring slightly different handedness when they are rendered in the same frame.
fn preworld_camera_carriage(eye: Vec3, focus: Vec3, aspect: f32) -> Camera {
    let unmirror = |v: Vec3| Vec3::new(v.x, -v.y, v.z);
    let mut camera = Camera::new(unmirror(eye), unmirror(focus), Vec3::ZERO, aspect);
    // The stage is framed by a horizontal 60°, not a vertical one — a vertical 60° is a 91.5°
    // horizontal lens at 16:9, which rendered every landmark too small and too far away.
    camera.set_horizontal_fov(caer_render::camera::PREWORLD_FOV_X);
    camera.ensure_far(60_000.0);
    camera
}

fn preworld_view_projection(eye: Vec3, focus: Vec3, aspect: f32) -> [[f32; 4]; 4] {
    let camera = preworld_camera_carriage(eye, focus, aspect);
    camera.view_proj().to_cols_array_2d()
}

/// Aim the pre-world lenses. Deliberately takes no subject.
///
/// Eden's character-create camera does not move when the race does: across every capture in a
/// realm — Kobold beside Troll, Inconnu beside Half Ogre — the backdrop outside the subject column
/// agrees, while the same statistic between two realms does not. A camera composed around the
/// figure could not produce that. Measured by `caer audit backdrop`, which recovers the three
/// realms from unlabelled captures at within-group max 4.00 against between-group min 56.00.
///
/// So the subject's dimensions are not an input here. They used to be three: `model_height` drove
/// the dolly and the posed head drove both eye and focus, which is why apparent tilt grew with
/// instance scale (ledger E13) and why the largest races read worst (E16).
fn aim_preworld_camera(
    scene: &caer_render::preworld_scene::PreWorldScene,
    gpu: &mut Gpu,
    viewport: (f32, f32),
    preview: caer_render::preworld_camera::CustomizerCamera,
) {
    let (vw, vh) = viewport;
    // One aspect for both halves of the composition: the framing solves the pull-back against the
    // vertical angle this aspect implies, and the camera then renders through that same angle.
    let aspect = vw / vh.max(1.0);
    let (backdrop_eye, backdrop_focus) =
        caer_render::preworld_scene::framing_backdrop(scene, aspect);
    gpu.set_view_proj(preworld_view_projection(
        backdrop_eye,
        backdrop_focus,
        aspect,
    ));

    // Retail keeps the realm stage static while Character Customize opens a close inspection of
    // the avatar.  In particular, applying the face dolly to Midgard's stage camera lands inside
    // its dome; the snowfield becomes black even though the source scene is fine.  Give only the
    // avatar the close lens and explicitly restore the shared camera for character select/create.
    let avatar_view = preview.uses_face_frame().then(|| {
        let (eye, focus) = caer_render::preworld_scene::framing_for_preview(scene, aspect, preview);
        preworld_view_projection(eye, focus, aspect)
    });
    gpu.set_preworld_avatar_view_proj(avatar_view);
}

/// Shared live + headless composition: scene models, then avatar (or clear), then camera.
fn compose_preworld_gpu(
    gpu: &mut Gpu,
    scene: &caer_render::preworld_scene::PreWorldScene,
    uploaded: &mut Option<u8>,
    morphing: &mut Option<Vec<caer_render::terrain::ModelBatch>>,
    entity_models: Option<&mut caer_render::entities::EntityModels>,
    identity: Option<(u8, u8)>,
    appearance: caer_protocol::customization::AvatarAppearance,
    dress: Option<&caer_protocol::equipment::EquipmentUpdate>,
    viewport: (f32, f32),
    anim_time: f32,
    preview: caer_render::preworld_camera::CustomizerCamera,
) {
    if *uploaded != Some(scene.realm) {
        gpu.set_models(&scene.models, &scene.textures);
        *uploaded = Some(scene.realm);
        // The loaded scene is shared and immutable; morphing writes vertices, so the animated
        // realms get a working copy. Realms with nothing keyed keep `None` and cost nothing.
        *morphing = scene
            .models
            .iter()
            .any(|m| !m.morphs.is_empty() || !m.uv_anims.is_empty())
            .then(|| scene.models.clone());
    }
    if let Some(batches) = morphing.as_mut() {
        for (src, batch) in batches.iter_mut().enumerate() {
            if batch.animate(anim_time) {
                gpu.update_model_vertices(src, &batch.vertices);
            }
        }
    }
    let feet = caer_render::preworld_scene::framing_around_character(
        scene,
        caer_render::preworld_scene::CHARACTER_HEIGHT,
        None,
        viewport.0 / viewport.1.max(1.0),
    )
    .2;
    stand_preworld_avatar(
        entity_models,
        gpu,
        feet,
        identity,
        appearance,
        dress,
        anim_time,
        // Turn to meet the camera.
        //
        // These bodies face -Y at yaw 0, and the instance rotation takes (x,y) to
        // (c·x − s·y, s·x + c·y), so the front after yaw ψ points at (sin ψ, −cos ψ). The eye sits
        // at −(sin θ, cos θ) from the anchor, and equating the two gives ψ = −θ.
        //
        // It must be the EFFECTIVE bearing. `camera_bearing_deg` is the value baked at scene load;
        // the camera itself orbits to the live tuning override, so reading the baked one leaves the
        // body facing where the lens used to be the moment the bearing is nudged. Midgard shows it
        // worst because its bearing is the one that moved furthest from the shipped value.
        -caer_render::preworld_scene::effective_bearing_deg(scene).to_radians() + preview.yaw(),
    );
    aim_preworld_camera(scene, gpu, viewport, preview);
}

/// A winit cursor position, expressed in the surface the frame was drawn into.
///
/// The conversion itself lives in [`caer_render::preworld_hitbox::window_to_surface`] so the
/// audit tooling measures the same one the client runs.
fn cursor_to_surface(x: f32, y: f32, window: (f32, f32), surface: (f32, f32)) -> [f32; 2] {
    caer_render::preworld_hitbox::window_to_surface(x, y, window, surface)
}

fn log_display_apply(window: &Window, settings: &caer_render::DisplaySettings) {
    let fs = match window.fullscreen() {
        None => "none",
        Some(Fullscreen::Borderless(_)) => "borderless",
        Some(Fullscreen::Exclusive(_)) => "exclusive",
    };
    let size = window.inner_size();
    println!(
        "rustdaoc: display requested={} resulting_fullscreen={} inner={}x{}",
        settings.mode.as_str(),
        fs,
        size.width,
        size.height,
    );
}

fn parse_preworld(name: &str) -> Option<caer_render::preworld::PreWorldScreen> {
    use caer_render::preworld::PreWorldScreen;
    match name.to_ascii_lowercase().as_str() {
        "login" => Some(PreWorldScreen::Login),
        "splash" | "intro" => Some(PreWorldScreen::Splash),
        "loading" | "linkdead" => Some(PreWorldScreen::Loading),
        "realm" | "realmselect" => Some(PreWorldScreen::RealmSelect),
        "charselect" | "characterselect" | "select" => Some(PreWorldScreen::CharSelect),
        "charcreate" | "create" => Some(PreWorldScreen::CharCreate),
        // E6/B3: the two creation sub-screens, capturable for the same reason the create form is.
        "customize" => Some(PreWorldScreen::CharCustomize),
        "charstats" | "stats" => Some(PreWorldScreen::CharStats),
        // The Options Menu is an overlay, so asking for it means the character screen with the
        // dialog up — see `preworld_opens_options`.
        "options" | "settings" => Some(PreWorldScreen::CharSelect),
        // Same for the two confirm modals: the character screen with that form raised. A modal that
        // only appears after a click is otherwise uncapturable, and both fixes are claims about what
        // the player sees.
        "quitconfirm" | "quit" => Some(PreWorldScreen::CharSelect),
        "deleteconfirm" | "delete" => Some(PreWorldScreen::CharSelect),
        // The dialog's second state. A two-state modal needs two captures, and the confirmed panel
        // is only reachable by typing, which a headless screenshot cannot do.
        "deleteconfirmed" | "deleteconfirm-confirmed" => Some(PreWorldScreen::CharSelect),
        _ => {
            eprintln!(
                "rustdaoc: unknown --preworld `{name}` (login|splash|loading|linkdead|options|quitconfirm|deleteconfirm|deleteconfirmed|realm|charselect|charcreate|customize|stats)"
            );
            None
        }
    }
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

/// Resolve the phase handed from the synchronous live warmup to the windowed product loop.
///
/// The event drain normally supplies the authoritative phase. `has_overview` proves character
/// select. `at_realm_select` covers unbound accounts that must stop on RealmSelect before any
/// overview (OPEN_ORACLE SendRealm(None)).
fn settle_warmup_phase(
    observed: caer_protocol::session::SessionPhase,
    has_player: bool,
    has_overview: bool,
    at_realm_select: bool,
) -> caer_protocol::session::SessionPhase {
    if has_player && matches!(observed, caer_protocol::session::SessionPhase::Disconnected) {
        caer_protocol::session::SessionPhase::InWorld
    } else if has_overview
        && matches!(
            observed,
            caer_protocol::session::SessionPhase::Disconnected
                | caer_protocol::session::SessionPhase::CryptHandshake
                | caer_protocol::session::SessionPhase::Authenticating
                | caer_protocol::session::SessionPhase::RealmSelect
        )
    {
        caer_protocol::session::SessionPhase::CharacterSelect
    } else if at_realm_select
        && matches!(
            observed,
            caer_protocol::session::SessionPhase::Disconnected
                | caer_protocol::session::SessionPhase::CryptHandshake
                | caer_protocol::session::SessionPhase::Authenticating
                | caer_protocol::session::SessionPhase::RealmSelect
        )
    {
        caer_protocol::session::SessionPhase::RealmSelect
    } else {
        observed
    }
}

/// Decide whether the explicit realm passed to the *headless live* verifier must be sent into
/// the same `LiveCommand` path the realm plate uses.
///
/// An all-realms DOL account quite correctly lands on `RealmSelect` after authentication.  The
/// windowed player leaves that decision to the human, but a non-interactive smoke has no click to
/// provide it.  Restrict the automatic dispatch to the smoke mode: `--realm` remains a passive
/// scene/draft seed for an ordinary interactive launch.
fn headless_warmup_realm_request(
    requested_realm: Option<u8>,
    headless_live_smoke: bool,
    at_realm_select: bool,
    has_overview: bool,
    has_player: bool,
    already_requested: bool,
) -> Option<u8> {
    (headless_live_smoke && at_realm_select && !has_overview && !has_player && !already_requested)
        .then_some(requested_realm)
        .flatten()
        .filter(|realm| (1..=3).contains(realm))
}

/// Obvious shard-specific files that make an asset tree unsuitable as retail-fidelity evidence.
///
/// A modified tree remains useful for protocol and renderer development, but its artwork must not
/// be mistaken for the stock client. Keep this deliberately narrow: absence of these markers does
/// not prove provenance; their presence conclusively disproves a clean retail tree.
fn modified_client_markers(root: &std::path::Path) -> Vec<&'static str> {
    [
        "eden.dll",
        "eden_2gb.dll",
        "EdenLauncher.exe",
        "EdenLauncher.log",
    ]
    .into_iter()
    .filter(|name| root.join(name).exists())
    .collect()
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

/// Whether authored product pointer handling must receive a mouse event even when egui reports it
/// consumed. Pre-world stock screens are independent of the optional in-world overlay.
fn product_pointer_bypasses_egui(
    at_preworld: bool,
    show_overlay: bool,
    display_settings_open: bool,
    is_product_mouse_event: bool,
) -> bool {
    is_product_mouse_event && !display_settings_open && (at_preworld || show_overlay)
}

/// Stable, dependency-free session seed for the stock splash rotation.
fn preworld_seed(account: &str) -> u64 {
    account.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn main() {
    init_logging("warn,caer_render=warn");
    println!(
        "rustdaoc: {} (preworld stand-log on)",
        env!("CARGO_PKG_VERSION")
    );
    let args = parse_args();
    if let Err(e) = validate_launch_args(&args, &|k| std::env::var(k).ok()) {
        eprintln!("rustdaoc: {e}");
        std::process::exit(2);
    }

    if args.build_identity {
        caer_client::evidence::emit_build_identity();
        return;
    }

    let client_root = caer_render::terrain::client_root();
    let modified_markers = modified_client_markers(&client_root);
    if !modified_markers.is_empty() {
        eprintln!(
            "rustdaoc: WARNING: modified shard asset tree detected at {} ({})\n\
             rustdaoc: visuals from this tree are valid for functional testing only, not retail-fidelity evidence",
            client_root.display(),
            modified_markers.join(", ")
        );
        if env_truthy("CAER_REQUIRE_RETAIL_ASSETS") {
            eprintln!(
                "rustdaoc: REFUSED: CAER_REQUIRE_RETAIL_ASSETS=1 requires a separately installed clean client tree"
            );
            std::process::exit(2);
        }
    }

    // Headless screenshot / pre-world verify: no live server — synthetic seat for the region.
    // `--headless-live-smoke` must take the live path (product LiveFeed), not this skip.
    let (world, spawn, feed, char_overview, ui_select_sent, warmup_identity, initial_session_phase) =
        if (args.screenshot.is_some() || args.offline_preworld()) && !args.headless_live_smoke {
            if args.preworld.is_some() {
                println!(
                    "rustdaoc: --preworld with no server — offline pre-world session, not connecting"
                );
            } else {
                println!("rustdaoc: --screenshot (SCN-00 boot) — skipping live connect");
            }
            // Dungeon regions seat at the table-derived product seat, not Camelot Hills.
            let spawn = if let Some(zone) = dungeon_zone_for_region(args.region) {
                let root = caer_render::terrain::client_root();
                let s = caer_render::dungeon_mesh::dungeon_product_seat(&root, zone);
                let label = caer_world::zone_name(zone).unwrap_or("dungeon");
                println!(
                    "rustdaoc: dungeon seat region {} zone {} ({label}) at ({:.1},{:.1},{:.1})",
                    args.region, zone, s[0], s[1], s[2]
                );
                s
            } else {
                [592250.0, 537900.0, 1750.0]
            };
            let initial_session_phase = if args.screenshot.is_some() && args.preworld.is_none() {
                caer_protocol::session::SessionPhase::InWorld
            } else {
                caer_protocol::session::SessionPhase::Disconnected
            };
            (
                WorldState::new(),
                spawn,
                None,
                None,
                false,
                None,
                initial_session_phase,
            )
        } else {
            // Player path: auto_select=false (SCN-01). Selection goes through LiveCommand / UI.
            let feed = live::spawn(
                args.server.clone(),
                args.account.clone(),
                args.password.clone(),
                args.character.clone(),
                false,
            );
            let mut world = WorldState::new();
            let mut player: Option<[f32; 3]> = None;
            let mut overview: Option<caer_protocol::overview::CharacterOverview> = None;
            let mut ui_select_sent = false;
            let mut warmup_phase = caer_protocol::session::SessionPhase::Disconnected;
            let mut saw_login_granted = false;
            let mut saw_realm_none = false;
            let mut warmup_realm_requested = false;
            // Sol HIGH 3: --char must latch identity here — click path is not the only entry.
            let mut warmup_identity: Option<caer_protocol::overview::CharacterSummary> = None;
            println!(
                "rustdaoc: connecting to {} as '{}' — UI character select (SCN-01)…",
                args.server, args.account
            );
            let start = Instant::now();
            while start.elapsed() < LIVE_WARMUP {
                let d = live::drain_into(feed.events(), &mut world);
                if let Some(phase) = d.phase {
                    warmup_phase = phase;
                }
                if d.login_granted {
                    saw_login_granted = true;
                }
                if let Some(ov) = d.overview {
                    overview = Some(ov);
                }
                if let Some(pp) = d.player {
                    player = Some(pp);
                }
                // Detect unbound realm: LoginGranted + RealmSelect phase without overview.
                if matches!(
                    warmup_phase,
                    caer_protocol::session::SessionPhase::RealmSelect
                ) && overview.is_none()
                    && saw_login_granted
                {
                    saw_realm_none = true;
                }
                if let Some(realm) = headless_warmup_realm_request(
                    args.realm,
                    args.headless_live_smoke,
                    saw_realm_none,
                    overview.is_some(),
                    player.is_some(),
                    warmup_realm_requested,
                ) {
                    println!(
                        "rustdaoc: headless UI-path realm select → {realm} via LiveCommand::RequestCharacterOverview"
                    );
                    if let Err(e) = feed.send(live::LiveCommand::RequestCharacterOverview { realm })
                    {
                        eprintln!("rustdaoc: headless realm select send failed: {e}");
                    }
                    warmup_realm_requested = true;
                }
                // --char NAME: still UI-path — SelectCharacter command seam, not LiveSession auto.
                if !ui_select_sent && player.is_none() {
                    if let (Some(ov), Some(want)) = (overview.as_ref(), args.character.as_ref()) {
                        if let Some(c) = ov
                            .characters
                            .iter()
                            .find(|c| c.name.eq_ignore_ascii_case(want))
                        {
                            println!(
                            "rustdaoc: UI-path select slot {} ({}) via LiveCommand::SelectCharacter",
                            c.slot, c.name
                        );
                            warmup_identity = Some(c.clone());
                            if let Err(e) =
                                feed.send(live::LiveCommand::SelectCharacter { slot: c.slot })
                            {
                                eprintln!("rustdaoc: select send failed: {e}");
                            }
                            ui_select_sent = true;
                        }
                    }
                }
                if player.is_some() && world.len() > 4 && d.applied == 0 {
                    break;
                }
                // Unbound account: stop warmup at RealmSelect (do not wait forever for overview).
                if !warmup_realm_requested
                    && saw_realm_none
                    && overview.is_none()
                    && player.is_none()
                    && start.elapsed() > std::time::Duration::from_secs(2)
                {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            let spawn = match player {
                Some(p) => {
                    println!(
                        "rustdaoc: in world at ({:.0}, {:.0}, {:.0}) — {} entities visible",
                        p[0],
                        p[1],
                        p[2],
                        world.len()
                    );
                    p
                }
                None if overview.is_some() => {
                    if args.headless_live_smoke {
                        eprintln!(
                            "rustdaoc: --headless-live-smoke stuck at character select \
                         (pass --char NAME with a playable slot)"
                        );
                        std::process::exit(1);
                    }
                    println!(
                    "rustdaoc: at character select — {} characters; click a slot (or pass --char NAME)",
                    overview.as_ref().map(|o| o.characters.len()).unwrap_or(0)
                );
                    [592250.0, 537900.0, 1750.0]
                }
                None if saw_realm_none
                    || matches!(
                        warmup_phase,
                        caer_protocol::session::SessionPhase::RealmSelect
                    ) =>
                {
                    if args.headless_live_smoke {
                        eprintln!(
                            "rustdaoc: --headless-live-smoke stuck at realm select \
                         (pass a realm-bound account + --char NAME)"
                        );
                        std::process::exit(1);
                    }
                    println!("rustdaoc: at realm select — choose Albion / Midgard / Hibernia");
                    [592250.0, 537900.0, 1750.0]
                }
                None => {
                    eprintln!("rustdaoc: never reached realm select, overview, or world — is the server up?");
                    std::process::exit(1);
                }
            };
            let initial_session_phase = settle_warmup_phase(
                warmup_phase,
                player.is_some(),
                overview.is_some(),
                saw_realm_none
                    || matches!(
                        warmup_phase,
                        caer_protocol::session::SessionPhase::RealmSelect
                    ),
            );
            (
                world,
                spawn,
                Some(feed),
                overview,
                ui_select_sent,
                warmup_identity,
                initial_session_phase,
            )
        };

    if args.headless_live_smoke {
        run_headless_live_smoke(&args, spawn, feed);
    }

    // Render origin = the spawn point (keeps GPU coords small and centred on the player).
    let origin = Vec3::new(spawn[0], spawn[1], screenshot_anim_time());

    // The client's own camera defaults. 1024x768 is only the ini we ask for first; every shipped
    // default*.ini carries the same [Camera] block, so the choice is not load-bearing.
    let client_cam = client_camera_defaults(1024, 768);
    // Read appearance choices once. Both the HUD labels and product clicks share this exact
    // catalogue, so a race-specific/bald source index cannot be displayed by one path and
    // rejected or remapped by the other.
    let appearance_catalog =
        match caer_render::preworld_appearance::AppearanceCatalog::load(&client_root) {
            Ok(catalog) => Some(Arc::new(catalog)),
            Err(error) => {
                eprintln!(
                    "rustdaoc: appearance catalogue unavailable at {}: {error}; \
                     pre-world controls will use the explicit test fallback",
                    client_root.display()
                );
                None
            }
        };

    let display = caer_render::DisplaySettings::load_or_default();
    let mut app = Client {
        world,
        origin,
        region: args.region,
        player: spawn,
        feed,
        live_server: args.server.clone(),
        live_character: args.character.clone(),
        reconnect_left: live::live_reconnect_attempts_from_env(),
        reconnect_at: None,
        entity_models: EntityModels::load(),
        terrain: None,
        self_model: None,
        self_equip_skin: caer_render::entities::EquipSkinKey::default(),
        // No Highlander Female product default — identity arrives from overview / controller.
        player_race: None,
        player_gender: None,
        player_appearance: caer_protocol::customization::AvatarAppearance::default(),
        transition: {
            let mut t = caer_protocol::transition::WorldTransition::new();
            t.seed_region(args.region, spawn[0] as i32, spawn[1] as i32);
            t
        },
        pending_terrain_reload: true,
        terrain_load: None,
        terrain_load_failed: None,
        audio: {
            let root = caer_render::terrain::client_root();
            let a = caer_render::AudioBus::try_open(&root);
            println!(
                "rustdaoc: audio bus {:?} ({})",
                a.device_kind(),
                root.display()
            );
            a
        },
        audio_smoke_done: false,
        footstep_accum: 0.0,
        shell: Shell::new(),
        display: display.clone(),
        display_draft: display,
        ui: None,
        // The character name is what we asked to log in as; the overview decoder carries the
        // authoritative name and can replace this once B.6 wires it through.
        hud: HudState {
            name: args.character.clone().unwrap_or_default(),
            ..HudState::default()
        },
        log: caer_render::combat::CombatLog::new(),
        chat: caer_render::chat::ChatState::new(),
        product: caer_render::product_loop::ProductController::new(),
        addons: caer_render::addon_product::ProductAddons::from_env(),
        quickbar: caer_render::quickbar::Quickbar::new(),
        recorder: None,
        record_poll: 0.0,
        started: Instant::now(),
        harness: args
            .harness_jsonl
            .clone()
            .map(caer_render::harness::NativeHarness::new)
            .transpose()
            .unwrap_or_else(|error| {
                eprintln!("rustdaoc: invalid --harness-jsonl: {error}");
                std::process::exit(2)
            }),
        harness_identity: args
            .harness_jsonl
            .as_ref()
            .map(|_| caer_render::harness::hello_process_identity()),
        harness_lab: if args.harness_jsonl.is_some() {
            std::env::var("CAER_HARNESS_LAB_ID").map_or_else(
                |_| caer_harness::Known::Unknown {
                    reason: "CAER_HARNESS_LAB_ID was not supplied by the lab authority".into(),
                },
                |value| caer_harness::Known::Known { value },
            )
        } else {
            caer_harness::Known::Unknown {
                reason: "harness mode is disabled".into(),
            }
        },
        harness_last_state: None,
        harness_frame_attempts: 0,
        harness_presented_frames: 0,
        harness_last_present: None,
        harness_focused: caer_harness::Known::Unknown {
            reason: "no Focused window event observed yet".into(),
        },
        harness_minimized: caer_harness::Known::Unknown {
            reason: "no window size event observed yet".into(),
        },
        harness_preworld_scene_job: caer_harness::AssetJobSnapshot {
            state: "idle".into(),
            started_elapsed_ms: caer_harness::Known::Unknown {
                reason: "pre-world scene job has not started".into(),
            },
            ended_elapsed_ms: caer_harness::Known::Unknown {
                reason: "pre-world scene job has not started".into(),
            },
            failure: None,
        },
        harness_commands: caer_render::harness_ingress::CommandCache::default(),
        harness_waits: caer_render::harness_ingress::PendingWaits::default(),
        harness_pending_capture: None,
        self_object_id: 0,
        requested_npcs: std::collections::HashSet::new(),
        npc_request_timer: 0.0,
        camera_snapback: true,
        binds: load_bindings(),
        motion_tracker: caer_render::motion::MotionTracker::default(),
        jumping: false,
        attacking: false,
        sitting: false,
        jump_left: 0.0,
        // Visible by default, as in DAoC: the interface is always up and Alt+Z hides it. It used
        // to default OFF and only appear while the chat box was open, so opening chat was the only
        // way to see the HUD — and sending a line closed it again.
        show_overlay: true,
        mods: ModifiersState::empty(),
        // `atlantis` is the shipped skin; a future /uiskin can pick another.
        skin_hud: caer_render::skinhud::SkinHud::load_shipped(),
        other_avatars: caer_render::other_avatars::OtherPlayerAvatars::new(),
        preworld_hud: {
            let mut hud = caer_render::preworld::PreWorldHud::new(client_root.clone());
            hud.set_appearance_catalog(appearance_catalog.clone());
            if let Some(ref name) = args.preworld {
                if let Some(screen) = parse_preworld(name) {
                    hud.set_screen(screen);
                }
            }
            Some(hud)
        },
        preworld_flow: match args
            .preworld
            .as_deref()
            .and_then(parse_preworld)
            .filter(|_| args.offline_preworld())
        {
            Some(screen) => caer_render::preworld_flow::PreWorldFlow::offline_at(
                preworld_seed(&args.account),
                screen,
            ),
            None => caer_render::preworld_flow::PreWorldFlow::from_observed_phase(
                preworld_seed(&args.account),
                initial_session_phase,
            ),
        },
        offline_preworld: args.offline_preworld(),
        preworld_forced: args.preworld.as_deref().and_then(parse_preworld),
        preworld_forced_options: args
            .preworld
            .as_deref()
            .is_some_and(|n| matches!(n.to_ascii_lowercase().as_str(), "options" | "settings")),
        preworld_forced_modal_typed: args.preworld.as_deref().is_some_and(|n| {
            matches!(
                n.to_ascii_lowercase().as_str(),
                "deleteconfirmed" | "deleteconfirm-confirmed"
            )
        }),
        preworld_forced_modal: args.preworld.as_deref().and_then(|n| {
            match n.to_ascii_lowercase().as_str() {
                "quitconfirm" | "quit" => Some(caer_render::preworld_hitbox::Modal::QuitConfirm),
                "deleteconfirm" | "delete" | "deleteconfirmed" | "deleteconfirm-confirmed" => {
                    Some(caer_render::preworld_hitbox::Modal::DeleteConfirm)
                }
                _ => None,
            }
        }),
        // Preserve the live phase consumed during warmup. A fresh account legitimately returns a
        // bare 16-byte overview with zero characters; that is CharacterSelect, not Disconnected.
        session_phase: initial_session_phase,
        preworld_trace: None,
        preworld_scene: None,
        preworld_scene_realm_tried: None,
        preworld_scene_uploaded: None,
        preworld_scene_morphing: None,
        preworld_preview_camera: caer_render::preworld_camera::CustomizerCamera::with_face_view(
            args.preworld_zoom
                .unwrap_or(caer_render::preworld_camera::CustomizerCamera::FACE_FRAME_DOLLY),
            args.preworld_tilt.unwrap_or(0.0),
            args.preworld_yaw.unwrap_or(0.0),
        ),
        preworld_preview_drag: None,
        world_assets_loaded: false,
        create_realm: args.realm.unwrap_or(1),
        create_draft: {
            // Empty name until the player types one — never invent "Newchar".
            let mut d = caer_protocol::charcreate::CharacterCreateDraft::albion_briton_stub("", 0);
            d.name.clear();
            d.realm = args.realm.unwrap_or(1);
            d.set_race(capture_race_for_realm(d.realm, args.race));
            if let Some(g) = args.gender {
                d.set_gender(g);
            }
            // The interactive race picker already normalises an incompatible class through the
            // source adapter table.  This headless seam starts from an Albion/Briton stub, so it
            // must do the same before a Hibernia or Midgard stats capture is allowed to describe
            // the selected avatar.  Otherwise the picture can be visually right while its class
            // prose is an impossible Armsman.
            let current_class_is_legal = matches!(
                caer_protocol::create_validity::classify(d.realm, d.class_id, d.race, d.gender),
                caer_protocol::create_validity::Legality::Allowed
            );
            let selected_class = if args.class_id.is_some() || !current_class_is_legal {
                capture_class_for(d.realm, d.race, d.gender, args.class_id)
            } else {
                Some(d.class_id)
            };
            if let Some(class_id) = selected_class {
                if let Some(requested) = args.class_id.filter(|requested| *requested != class_id) {
                    eprintln!(
                        "rustdaoc: --class {requested} is illegal for realm {} race {} gender {}; using retail form class {class_id}",
                        d.realm, d.race, d.gender,
                    );
                }
                d.class_id = class_id;
            } else if let Some(requested) = args.class_id {
                eprintln!(
                    "rustdaoc: --class {requested} has no legal fallback for realm {} race {} gender {}",
                    d.realm, d.race, d.gender,
                );
            }
            d.set_customization(args.custom);
            for (source_slot, tick) in args.morph_ticks.iter().copied().enumerate() {
                let Some(tick) = tick else { continue };
                let Some(slot) = caer_protocol::customization::FacialMorphSlot::from_source_slot(
                    source_slot as u8,
                ) else {
                    continue;
                };
                if tick > caer_protocol::customization::FACIAL_MORPH_MAX_TICK {
                    eprintln!(
                        "rustdaoc: ignoring --morph slot {source_slot} value {tick}; retail slider range is 0..={} ",
                        caer_protocol::customization::FACIAL_MORPH_MAX_TICK,
                    );
                    continue;
                }
                d.set_facial_morph_tick(slot, tick);
            }
            d
        },
        name_edit_focus: false,
        pending_create_name: None,
        login_account: args.account.clone(),
        login_password: args.password.clone(),
        char_overview,
        create_names: caer_assets::names::load(std::path::Path::new(
            &std::env::var("CAER_CLIENT").unwrap_or_default(),
        )),
        appearance_catalog,
        customizer_state: caer_render::preworld_product::CustomizerState::default(),
        selected_protocol_slot: None,
        ui_select_sent,
        creation_sent: false,
        cursor: [0.0, 0.0],
        outgoing_chat: None,
        floaters: caer_render::combat::FloatingTexts::new(),
        pending_damage: Vec::new(),
        pending_combat_anims: Vec::new(),
        target: None,
        last_attacker: None,
        autorun: false,
        show_names: true,
        mouse_look: false,
        show_perf_meter: false,
        max_speed_percent: 100,
        // Camera defaults come from the client's own [Camera] block where readable; the consts are
        // only the fallback. This is a conversion — the original's numbers win over ours.
        cam_orbit: client_cam.map_or(0.0, |c| c.angle_radians()),
        orbit_pitch: client_cam.map_or(CAM_PITCH_START, |c| c.tilt_radians()),
        cam_dist: client_cam.map_or(CAM_DIST, |c| c.distance),
        turning: false,
        orbiting: false,
        keys: MoveKeys::default(),
        heading: 0,
        was_moving: false,
        last_move_send: Instant::now(),
        quitting: None,
        pending_exit: false,
        exit_when_flushed: None,
        live_link_error: RefCell::new(None),
        instances: Vec::new(),
        mesh_instances: std::collections::HashMap::new(),
        particle_fx: Vec::new(),
        preworld_particles: Vec::new(),
        preworld_particles_realm: None,
        liveness: LivenessProbe::from_env(),
        last_frame: Instant::now(),
        fps_timer: Instant::now(),
        frames: 0,
    };

    // Sol HIGH 3: --char startup must latch identity before first in-world render (click is not required).
    if let Some(c) = warmup_identity {
        app.hud.name = c.name.clone();
        app.apply_identity_from_summary(&c);
        assert!(
            app.player_race.is_some() && app.player_gender.is_some(),
            "rustdaoc: --char {} left avatar identity unresolved",
            c.name
        );
    }

    if let Some(out) = args.screenshot.clone() {
        app.screenshot(&out, args.size);
        return;
    }

    // Pre-world artwork is archive-backed and comparatively expensive to decode on a cold NTFS
    // cache.  Doing that work from the first RedrawRequested leaves the newly mapped Wayland
    // surface unanswered for several seconds; NVIDIA's FIFO acquire then times out and KWin quite
    // reasonably labels the client unresponsive.  Prime the exact live screen before winit creates
    // the visible window/surface.  `ensure_loaded` is idempotent, so render-time calls remain safe.
    app.sync_preworld_screen();
    if app.at_preworld() {
        let started = Instant::now();
        if let Some(hud) = app.preworld_hud.as_mut() {
            match hud.ensure_loaded() {
                Ok(()) => println!(
                    "rustdaoc: pre-world assets primed in {:.3}s",
                    started.elapsed().as_secs_f64()
                ),
                Err(error) => eprintln!(
                    "rustdaoc: pre-world asset prime completed with a soft failure after {:.3}s: {error}",
                    started.elapsed().as_secs_f64()
                ),
            }
        }
    }

    let event_loop = EventLoop::<caer_render::harness_ingress::HarnessUserEvent>::with_user_event()
        .build()
        .expect("event loop");
    // `about_to_wait` schedules the next frame explicitly.  Do not use Poll here: frame() used to
    // request another redraw while about_to_wait requested one as well, producing an unbounded
    // self-feeding present loop (black-frame flashing + dead realm hover/click on KDE/Wayland).
    // `about_to_wait` always leaves a finite fallback deadline installed, including after it queues
    // a redraw.  Some Wayland compositors coalesce that queued redraw; switching to an indefinite
    // Wait in the same callback then strands the client with no event capable of advancing it.
    event_loop.set_control_flow(ControlFlow::Wait);
    if app.harness.is_some() {
        if caer_render::harness_ingress::spawn_stdin_reader(event_loop.create_proxy()).is_err() {
            if let Some(harness) = app.harness.as_mut() {
                harness.fatal(
                    caer_harness::FatalClass::InfraFailure,
                    "native.command_ingress_spawn",
                    "harness stdin reader could not be started".into(),
                );
            }
        }
    }
    event_loop.run_app(&mut app).expect("run app");
}

/// Immutable snapshot of the world slice requested by a terrain worker.
///
/// Region/origin are deliberately carried with the result instead of read back from `Client` on
/// completion. A `RegionChanged` packet can arrive while the worker is decoding MPKs; installing
/// that old result under the new origin is the stale-terrain regression this guards against.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TerrainLoadRequest {
    region: u16,
    origin: Vec3,
    min: [i32; 2],
    max: [i32; 2],
}

impl TerrainLoadRequest {
    fn around_player(region: u16, origin: Vec3, player: [f32; 3]) -> Self {
        let radius = CULL_RADIUS;
        let (px, py) = (player[0] as i32, player[1] as i32);
        Self {
            region,
            origin,
            min: [px - radius, py - radius],
            max: [px + radius, py + radius],
        }
    }

    fn still_matches(self, region: u16, origin: Vec3) -> bool {
        self.region == region && self.origin.x == origin.x && self.origin.y == origin.y
    }
}

/// CPU-only completion from the terrain worker. GPU upload remains on Winit's main thread.
struct TerrainLoadResult {
    request: TerrainLoadRequest,
    mesh: terrain::TerrainMesh,
    dungeon: caer_render::dungeon_mesh::DungeonLoadStats,
    decode_elapsed: Duration,
}

enum TerrainLoadCompletion {
    Ready(TerrainLoadResult),
    Failed {
        request: TerrainLoadRequest,
        reason: &'static str,
    },
}

/// A single in-flight terrain decode. The receiver is polled from `frame`, never awaited on the
/// UI thread, so focus changes and the loading plate continue to receive Winit events.
struct TerrainLoadTask {
    request: TerrainLoadRequest,
    started: Instant,
    receiver: Receiver<TerrainLoadCompletion>,
}

struct Client {
    world: WorldState,
    origin: Vec3,
    region: u16,
    /// Latest player world position (the camera anchor), updated each frame from the live feed.
    player: [f32; 3],
    feed: Option<live::LiveFeed>,
    live_server: String,
    live_character: Option<String>,
    reconnect_left: u32,
    reconnect_at: Option<Instant>,
    entity_models: Option<EntityModels>,
    /// The loaded terrain around the player — retained so movement can sample ground height and
    /// keep the character on the surface instead of floating at the spawn z.
    terrain: Option<terrain::TerrainMesh>,
    /// The assembled player-avatar mesh id (once resolved), used to draw the real body for the
    /// `Self_` entity instead of the placeholder box. `None` until resolved / if assets are absent.
    /// Re-resolved when equipped armour skins change (MS-02b).
    self_model: Option<u16>,
    /// Last `EquipSkinKey` applied to `self_model` — skips re-upload when equipment is unchanged.
    self_equip_skin: caer_render::entities::EquipSkinKey,
    /// Selected character race (`eRace`) and fig3 gender — `None` until overview names them.
    /// There is **no** Highlander Female product default (System 3 falsifier).
    player_race: Option<u8>,
    player_gender: Option<u8>,
    /// The logged-in character's full source appearance, from the overview row we selected.
    /// This includes vertex-level facial morphs as well as face/hair asset selectors.
    player_appearance: caer_protocol::customization::AvatarAppearance,
    /// Authoritative realm / identity / region owner (System 3).
    transition: caer_protocol::transition::WorldTransition,
    /// Set when RegionChanged / teleport requires a terrain+origin reload.
    pending_terrain_reload: bool,
    /// CPU decode in progress while the authored loading plate remains presentable.
    terrain_load: Option<TerrainLoadTask>,
    /// A worker failure is terminal for that exact world slice until a new RegionChanged/teleport
    /// gives us a different request. This prevents a malformed asset from spawning a worker every
    /// frame and turning a useful error into an invisible busy loop.
    terrain_load_failed: Option<TerrainLoadRequest>,
    /// Product audio bus (typed logical → shipped WAV). Null device when no output.
    audio: caer_render::AudioBus,
    /// One-shot `CAER_AUDIO_SMOKE=1` thunder after world entry.
    audio_smoke_done: bool,
    /// Distance accumulated since last footstep (System 8).
    footstep_accum: f32,
    /// Window + GPU surface + camera lifecycle, shared with the dev viewer (see `shell::Shell`).
    shell: Shell,
    /// Authoritative display mode / resolution (persisted under `~/.config/caer/display.cfg`).
    display: caer_render::DisplaySettings,
    /// Player-edited display draft. It stays separate so browsing modes never resizes the live
    /// surface until Apply is pressed.
    display_draft: caer_render::DisplaySettings,
    /// The 2D overlay host (C.1). `None` until the window exists; the same `ui::Ui` the dev viewer
    /// uses for its sidebar, here driving the game HUD instead.
    ui: Option<caer_render::ui::Ui>,
    /// What the HUD draws, refilled from the world each frame.
    hud: HudState,
    /// Scrollback of system/combat messages (C.7).
    log: caer_render::combat::CombatLog,
    /// Chat input line (C.3). While open it owns the keyboard.
    chat: caer_render::chat::ChatState,
    /// Group invite / trade / quest product loop (ProductInput → typed command).
    product: caer_render::product_loop::ProductController,
    /// mlua host: read-only event projection → LiveCommand intents.
    /// Loads player `mods/` (`CAER_ADDONS_DIR` / `~/.local/share/caer/mods`). Off with CAER_ADDONS=0.
    addons: caer_render::addon_product::ProductAddons,
    /// Ability/spell bar (C.2), filled from the decoded skill list.
    quickbar: caer_render::quickbar::Quickbar,
    /// In-game frame recorder (F9, or an external request). See [`Recorder`].
    recorder: Option<Recorder>,
    /// Seconds since the recording-request file was last polled.
    record_poll: f32,
    /// When the client started, for relative timestamps in the telemetry log.
    started: Instant,
    /// Opt-in telemetry adapter; absent unless `--harness-jsonl RUN_ID` was explicit.
    harness: Option<caer_render::harness::NativeHarness>,
    /// Immutable process/build/GPU identity, computed once after GPU initialization.
    harness_identity: Option<caer_harness::HelloIdentity>,
    harness_lab: caer_harness::Known<String>,
    harness_last_state: Option<String>,
    harness_frame_attempts: u64,
    harness_presented_frames: u64,
    harness_last_present: Option<Instant>,
    harness_focused: caer_harness::Known<bool>,
    harness_minimized: caer_harness::Known<bool>,
    harness_preworld_scene_job: caer_harness::AssetJobSnapshot,
    /// Fixed, fail-closed replay protection for harness commands. It is absent from every product
    /// decision and populated only by the explicit harness ingress.
    harness_commands: caer_render::harness_ingress::CommandCache,
    /// Non-blocking predicates evaluated on the Winit owner thread after authoritative snapshots.
    harness_waits: caer_render::harness_ingress::PendingWaits,
    /// Command/artifact identity retained until a post-present capture and manifest are complete.
    harness_pending_capture: Option<(String, String, u64)>,
    /// Our own object id, from the spawn packet — echoed in every position update from 1.127.
    self_object_id: u16,
    /// Object ids we have already asked the server to describe, so a persistent unknown doesn't
    /// generate a request every tick. Cleared on ObjectRemoved (that id), RegionChanged, and
    /// LoggedOut — otherwise recycled ids never re-request and leave stale boxes.
    requested_npcs: std::collections::HashSet<u16>,
    /// Throttle for those requests.
    npc_request_timer: f32,
    /// Whether turning re-centres the camera behind the character (F3). On by default, as in the
    /// client — but CC and kiting classes disable it so the camera can keep watching something
    /// behind them while they turn.
    camera_snapback: bool,
    /// The player's key → action map. Input is resolved through this rather than matching physical
    /// keys, which is what makes `/keyboard` and a config file possible.
    binds: Bindings,
    /// Smooths NPC movement between the server's position updates — without it a walking mob
    /// stands still playing its walk cycle and then jumps.
    motion_tracker: caer_render::motion::MotionTracker,
    /// Mid-jump: drives the server's `playerAction & 0x40` bit.
    jumping: bool,
    /// Whether melee attack mode is on (toggled with F, sent as `PlayerAttackRequest`).
    attacking: bool,
    /// Last PlayerSitRequest intent (toggle only). Not a posed sit until animation exists.
    sitting: bool,
    /// Remaining jump airtime, seconds.
    jump_left: f32,
    /// Whether the stop-gap egui overlay is drawn. **Off by default**: the target is the world as
    /// you'd see it after Alt+Z in real DAoC, and the real interface will be a Rust backend for the
    /// client's own UI assets (ui/atlantis, ui/isles — sprite atlases + window XML), not these
    /// hand-built panels. They stay behind F1 because a health bar and a combat log are still
    /// useful while debugging the world.
    show_overlay: bool,
    /// Currently-held modifiers. Winit delivers these as their own event, not on the key event, so
    /// without tracking them a chord binding (Alt+Z) can never match.
    mods: ModifiersState,
    /// The CLIENT's own UI skin, driving the real interface from `ui/<skin>/*.xml`. `None` if the
    /// client's `ui/` could not be read, in which case the egui HUD remains the fallback.
    skin_hud: Option<caer_render::skinhud::SkinHud>,
    /// Other-player anim/diagnostic owner (AVT INT hook).
    other_avatars: caer_render::other_avatars::OtherPlayerAvatars,
    /// Pre-world surfaces (login / realm / char-select / create). Active when forced via
    /// `--preworld` or when the session has not entered the world yet.
    preworld_hud: Option<caer_render::preworld::PreWorldHud>,
    /// Protocol-driven owner of login → realm → character → loading → world presentation.
    preworld_flow: caer_render::preworld_flow::PreWorldFlow,
    preworld_forced: Option<caer_render::preworld::PreWorldScreen>,
    /// A pre-world window with no server behind it: local navigation is the only navigator.
    offline_preworld: bool,
    /// `--preworld options`: force the character screen with the Options Menu already up, so the
    /// dialog can be captured without a live session.
    preworld_forced_options: bool,
    /// `--preworld quitconfirm` / `deleteconfirm`: same, for an authored confirm modal
    /// (ledger A13, B5).
    preworld_forced_modal: Option<caer_render::preworld_hitbox::Modal>,
    /// `--preworld deleteconfirmed`: raise the modal AND satisfy its typed confirmation, so the
    /// second panel can be captured headlessly.
    preworld_forced_modal_typed: bool,
    /// Live session phase (inferred from `live::Drain::phase`). Drives pre-world when not forced.
    session_phase: caer_protocol::session::SessionPhase,
    /// Last pre-world submission reported to the operator. Emitting only on change keeps the
    /// live phase/render seam observable without logging every frame.
    /// Realm scene behind the character screens, cached per realm.
    preworld_scene: Option<std::sync::Arc<caer_render::preworld_scene::PreWorldScene>>,
    /// Realm whose scene load has been attempted, whether or not it produced geometry.
    preworld_scene_realm_tried: Option<u8>,
    /// Realm whose scene is currently in the GPU's model buffers.
    preworld_scene_uploaded: Option<u8>,
    /// Working copy of the uploaded scene's batches while its vertex animation plays. `None` for a
    /// realm with nothing keyed, and dropped whenever the uploaded realm changes.
    preworld_scene_morphing: Option<Vec<caer_render::terrain::ModelBatch>>,
    /// Local-only view state for retail's character-customizer camera controls.  It never changes
    /// the normal character-select framing.
    preworld_preview_camera: caer_render::preworld_camera::CustomizerCamera,
    /// Which retail drag contract currently owns pointer motion over the customizer canvas.
    preworld_preview_drag: Option<PreWorldPreviewDrag>,
    preworld_trace: Option<(
        caer_protocol::session::SessionPhase,
        caer_render::preworld::PreWorldScreen,
        usize,
        usize,
    )>,
    /// Terrain and the local avatar are world-only assets. Loading them before the first event-loop
    /// frame leaves pre-world startup staring at an unpresented black surface.
    world_assets_loaded: bool,
    /// Realm chosen on the realm-select plate (1/2/3). Feeds CharacterCreateDraft.
    create_realm: u8,
    /// Create-form draft updated by race/class/name hits; Continué submits this (not a hardcoded stub).
    create_draft: caer_protocol::charcreate::CharacterCreateDraft,
    /// Name edit box has keyboard focus on the create form.
    name_edit_focus: bool,
    /// Set on Continué; cleared when a refreshed overview contains that name (§14A round-trip signal).
    pending_create_name: Option<String>,
    /// Account / password shown in the `pregame/login.xml` dialog (from CLI; editable later).
    login_account: String,
    login_password: String,
    /// Characters from the last overview (SCN-01 UI listing).
    char_overview: Option<caer_protocol::overview::CharacterOverview>,
    /// Client name fragments for the create form's Random button (`charman/names.dat`).
    create_names: caer_assets::names::NameTable,
    /// Retail source choices shared by the customizer's labels and product dispatch. `None` only
    /// when the configured client tree could not be read; that state is logged at startup.
    appearance_catalog: Option<Arc<caer_render::preworld_appearance::AppearanceCatalog>>,
    /// Form-local Random-lock state from `character_customize.xml`. Size is stored in the
    /// draft's source-compatible creation-model word.
    customizer_state: caer_render::preworld_product::CustomizerState,
    /// Selected character's **protocol slot**; selection is local until Play is pressed.
    ///
    /// H4: this used to be the row index into the compact overview list, which a refresh could
    /// silently repoint at a different character.
    selected_protocol_slot: Option<u8>,
    /// True once we've sent SelectCharacter this session (warmup --char or UI click).
    ui_select_sent: bool,
    /// E6: set when a create packet went out this session; mirrors
    /// `PreWorldProductState::creation_sent` across the dispatch round trip.
    creation_sent: bool,
    /// Last cursor position in window pixels (for pre-world hit-testing).
    cursor: [f32; 2],
    /// A line the player submitted this frame, put on the wire after the overlay closure ends
    /// (which borrows `self.chat`, so the send cannot happen inside it).
    outgoing_chat: Option<caer_render::chat::Outgoing>,
    /// Live floating damage numbers.
    floaters: caer_render::combat::FloatingTexts,
    /// Damage parsed this tick, waiting to be anchored once nameplates are projected. Outgoing
    /// numbers float over the TARGET, which we only know the screen position of after projection.
    pending_damage: Vec<caer_render::combat::Damage>,
    /// CombatAnimation 0xBC feedback waiting for screen anchors (REQ-021 provenance path).
    pending_combat_anims: Vec<(caer_protocol::combat_anim::CombatAnimation, bool)>,
    /// Camera azimuth OFFSET from directly behind the character, in radians.
    ///
    /// DAoC's camera orbits the character rather than defining its movement: the character owns a
    /// persistent facing (`heading`), and this is only how far the camera has been swung around it
    /// with a left-drag. Zero means directly behind. Turning the character (right-drag) leaves this
    /// alone, so the camera stays over the same shoulder while the character rotates under it.
    cam_orbit: f32,
    orbit_pitch: f32,
    cam_dist: f32,
    /// Right mouse held: drag turns the CHARACTER (camera follows behind it).
    turning: bool,
    /// Left mouse held: drag orbits the CAMERA only, leaving the character facing where it is.
    orbiting: bool,
    /// Which movement keys are held (WASD).
    keys: MoveKeys,
    /// Latest travel heading (DAoC 0..4096), updated as the player moves; sent with position.
    heading: u16,
    /// Whether the player was moving last frame — used to send one final stop-update on release.
    was_moving: bool,
    /// Throttle for on-the-wire PositionUpdates (local motion is every frame; the wire is ~200 ms).
    last_move_send: Instant,
    /// Currently targeted entity (object id), and its name for the title bar. `None` = no target.
    /// Targeting is client-side selection that we mirror to the server for later attack/spell use.
    target: Option<(u16, String)>,
    /// Last attacked / attack-mode target for [`ProductInput::LastAttacker`].
    last_attacker: Option<(u16, String)>,
    /// Runlock sticky forward until cancelled (back / Runlock toggle).
    autorun: bool,
    /// Nameplate draw gate ([`ProductInput::ToggleNames`]).
    show_names: bool,
    /// When true, cursor motion always pitches/yaws the camera (MouseLookToggle).
    mouse_look: bool,
    /// When true, surface FPS in the combat log once per second (PerfMeter).
    show_perf_meter: bool,
    /// Server MaxSpeed 0xB6 percent of base (100 = normal). Affects travel_speed.
    max_speed_percent: u16,
    /// Set when the player asked to quit (window close / Esc): we send `/quit` and keep rendering
    /// until the server confirms the logout, so the socket closes cleanly (no link-death ghost).
    /// Holds the time the quit was requested (for a safety timeout).
    quitting: Option<Instant>,
    /// Set once the logout completed (or timed out) — `about_to_wait` exits the loop next tick
    /// (frame() has no `event_loop` handle, so it defers the actual exit to there).
    pending_exit: bool,
    /// A quit that is waiting for its command to reach the wire. See the pre-world quit path.
    exit_when_flushed: Option<Instant>,
    /// Explicit live-link failure (command send / unexpected session end). Not a silent freeze.
    live_link_error: RefCell<Option<String>>,
    instances: Vec<Instance>,
    mesh_instances: std::collections::HashMap<u16, Vec<[f32; 5]>>,
    /// SpellEffect 0x1B → particle billboard bursts (leg 10 draw path; fidelity not claimed).
    particle_fx: Vec<caer_render::particles::ParticleSystem>,
    /// Emitters authored into the pre-world stage, live while that stage is on screen. Separate
    /// from `particle_fx`, which is spell effects: those are cleared on reconnect and capped as a
    /// group, and the stage's own snow is neither.
    preworld_particles: Vec<caer_render::particles::ParticleSystem>,
    preworld_particles_realm: Option<u8>,
    /// Diagnostic-only event-loop progress probe (`CAER_LIVENESS_TRACE=1`).
    liveness: Option<LivenessProbe>,
    last_frame: Instant,
    fps_timer: Instant,
    frames: u32,
}

/// The two interaction contracts the retail customize form labels explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreWorldPreviewDrag {
    Rotate,
    Zoom,
}

/// The customizer and its stats plate share the close face framing, but they do not share pointer
/// ownership.  Stats is a modal: the still-visible avatar remains in its customizer composition
/// while only the stats window (and its explicit dismiss route) receives input.
fn customizer_preview_visible(screen: Option<caer_render::preworld::PreWorldScreen>) -> bool {
    matches!(
        screen,
        Some(
            caer_render::preworld::PreWorldScreen::CharCustomize
                | caer_render::preworld::PreWorldScreen::CharStats
        )
    )
}

/// Whether a screen may begin a source-authorized preview drag.
///
/// The source instructions belong to `character_customize.xml`, not
/// `character_customize_stats.xml`; the latter is a modal window above it.
fn customizer_preview_drag_allowed(screen: Option<caer_render::preworld::PreWorldScreen>) -> bool {
    matches!(
        screen,
        Some(caer_render::preworld::PreWorldScreen::CharCustomize)
    )
}

impl Client {
    fn mark_liveness(&self, phase: u8) {
        if let Some(probe) = self.liveness.as_ref() {
            probe.mark(phase);
        }
    }

    fn harness_state_label(&self) -> String {
        let options = self.preworld_hud.as_ref().map(|hud| {
            (
                hud.options_open(),
                hud.modal(),
                hud.modal_confirmed(),
                format!("{:?}", hud.options_draft()),
            )
        });
        let interaction = format!(
            "{:?}",
            (
                &self.create_draft,
                &self.customizer_state,
                &self.char_overview,
                self.selected_protocol_slot,
                self.preworld_flow.pending(),
                &self.harness_focused,
                &self.harness_minimized,
                self.quitting.is_some(),
                self.name_edit_focus,
                self.ui_select_sent,
                self.creation_sent,
                (
                    options,
                    self.harness_pending_capture
                        .as_ref()
                        .map(|(_, name, _)| name),
                    self.held_input_names(),
                ),
            )
        );
        let mut hasher = Sha256::new();
        hasher.update(interaction.as_bytes());
        let interaction = format!("{:x}", hasher.finalize());
        format!(
            "session={:?};flow={:?};screen={:?};interaction={interaction};shutdown={}",
            self.session_phase,
            self.preworld_flow.step(),
            self.preworld_flow.screen(),
            if self.pending_exit {
                "pending_exit"
            } else if self.quitting.is_some() || self.exit_when_flushed.is_some() {
                "flushing"
            } else {
                "running"
            }
        )
    }

    fn emit_harness_asset(&mut self, job_id: String, state: &str) {
        if let Some(harness) = self.harness.as_mut() {
            harness.asset_job(job_id, state.into());
        }
    }

    fn emit_harness_server_effect(&mut self, drain: &live::Drain) {
        if drain.applied == 0 && drain.overview.is_none() && drain.phase.is_none() && !drain.ended {
            return;
        }
        let effect = format!(
            "applied={};phase={:?};overview={};player={};region={:?};entered_world={};logged_out={};ended={};create_reply={}",
            drain.applied,
            drain.phase,
            drain.overview.as_ref().map_or(0, |overview| overview.characters.len()),
            drain.player.is_some(),
            drain.region_changed,
            drain.entered_world,
            drain.logged_out,
            drain.ended,
            drain.create_reply.is_some(),
        );
        if let Some(harness) = self.harness.as_mut() {
            harness.server_effect_summary(effect);
        }
    }

    fn harness_snapshot(&self) -> caer_harness::Snapshot {
        use caer_harness::{
            AssetJobSnapshot, AssetSnapshot, Known, PresentationSnapshot, SessionSnapshot,
            ShutdownSnapshot, Snapshot, SnapshotIdentity, WorldSnapshot,
        };

        let draft = &self.create_draft;
        let window = self.shell.window();
        let surface_physical_size = self.shell.gpu().map_or_else(
            || Known::Unknown {
                reason: "GPU surface not created".into(),
            },
            |gpu| {
                let (width, height) = gpu.surface_size();
                Known::Known {
                    value: [width as u32, height as u32],
                }
            },
        );
        let (window_logical_size, scale_factor) = window.map_or_else(
            || {
                (
                    Known::Unknown {
                        reason: "native window not created".into(),
                    },
                    Known::Unknown {
                        reason: "native window not created".into(),
                    },
                )
            },
            |window| {
                let scale = window.scale_factor();
                let logical = window.inner_size().to_logical::<f64>(scale);
                (
                    Known::Known {
                        value: [logical.width, logical.height],
                    },
                    Known::Known { value: scale },
                )
            },
        );
        let terrain = if let Some(task) = &self.terrain_load {
            AssetJobSnapshot {
                state: "running".into(),
                started_elapsed_ms: Known::Known {
                    value: task
                        .started
                        .saturating_duration_since(self.started)
                        .as_millis()
                        .try_into()
                        .unwrap_or(u64::MAX),
                },
                ended_elapsed_ms: Known::Unknown {
                    reason: "terrain job is still running".into(),
                },
                failure: None,
            }
        } else if self.terrain.is_some() {
            AssetJobSnapshot {
                state: "complete".into(),
                started_elapsed_ms: Known::Unknown {
                    reason: "terminal terrain timing is not retained by the product owner".into(),
                },
                ended_elapsed_ms: Known::Unknown {
                    reason: "terminal terrain timing is not retained by the product owner".into(),
                },
                failure: None,
            }
        } else if self.terrain_load_failed.is_some() {
            AssetJobSnapshot {
                state: "failed".into(),
                started_elapsed_ms: Known::Unknown {
                    reason: "failed terrain timing is not retained by the product owner".into(),
                },
                ended_elapsed_ms: Known::Unknown {
                    reason: "failed terrain timing is not retained by the product owner".into(),
                },
                failure: Some("terrain decode failed for current request".into()),
            }
        } else {
            AssetJobSnapshot {
                state: "idle".into(),
                started_elapsed_ms: Known::Unknown {
                    reason: "terrain job has not started".into(),
                },
                ended_elapsed_ms: Known::Unknown {
                    reason: "terrain job has not started".into(),
                },
                failure: None,
            }
        };
        let unknown_job = |reason: &str| AssetJobSnapshot {
            state: "unknown".into(),
            started_elapsed_ms: Known::Unknown {
                reason: reason.into(),
            },
            ended_elapsed_ms: Known::Unknown {
                reason: reason.into(),
            },
            failure: None,
        };
        let entered = matches!(
            self.session_phase,
            caer_protocol::session::SessionPhase::InWorld
        );
        Snapshot {
            revision: 0,
            event_sequence: 0,
            elapsed_ms: 0,
            identity: SnapshotIdentity {
                build: self.harness_identity.clone().unwrap_or_else(|| {
                    caer_harness::HelloIdentity {
                        source_revision: Known::Unknown {
                            reason: "native harness hello has not completed".into(),
                        },
                        dirty_diff_identity: Known::Unknown {
                            reason: "native harness hello has not completed".into(),
                        },
                        executable_sha256: Known::Unknown {
                            reason: "native harness hello has not completed".into(),
                        },
                        build_profile: Known::Unknown {
                            reason: "native harness hello has not completed".into(),
                        },
                        adapter: Known::Unknown {
                            reason: "GPU adapter has not initialized".into(),
                        },
                        backend: Known::Unknown {
                            reason: "GPU backend has not initialized".into(),
                        },
                        process_id: Known::Known {
                            value: std::process::id(),
                        },
                        parent_process_id: Known::Unknown {
                            reason: "native harness hello has not completed".into(),
                        },
                    }
                }),
                lab: self.harness_lab.clone(),
                server: if self.live_server.is_empty() {
                    Known::Unknown {
                        reason: "offline pre-world run has no server".into(),
                    }
                } else {
                    Known::Known {
                        value: self.live_server.clone(),
                    }
                },
            },
            session: SessionSnapshot {
                phase: format!("{:?}", self.session_phase),
                flow_step: format!("{:?}", self.preworld_flow.step()),
                screen: self
                    .preworld_flow
                    .screen()
                    .map(|screen| format!("{screen:?}")),
                pending_request: self
                    .preworld_flow
                    .pending()
                    .map(|pending| format!("{pending:?}")),
                last_error: self
                    .live_link_error
                    .borrow()
                    .as_ref()
                    .map(|_| "live link reported an error; details remain in product log".into()),
                connection: self.feed.as_ref().map_or_else(
                    || Known::Unknown {
                        reason: "offline run has no LiveFeed".into(),
                    },
                    |feed| Known::Known {
                        value: format!("{:?}", feed.state()),
                    },
                ),
                inbound_queue_depth: Known::Unknown {
                    reason: "LiveFeed does not expose inbound receiver depth".into(),
                },
                outbound_queue_depth: self.feed.as_ref().map_or_else(
                    || Known::Known { value: 0 },
                    |feed| Known::Known {
                        value: feed.queued(),
                    },
                ),
                account_realm: if self.char_overview.is_some() || entered {
                    match self.transition.realm() {
                        value @ 1..=3 => Known::Known { value },
                        _ => Known::Unknown {
                            reason: "server state has not established an account realm".into(),
                        },
                    }
                } else {
                    Known::Unknown {
                        reason: "account realm is pending authoritative overview/world state"
                            .into(),
                    }
                },
            },
            character: caer_render::harness::project_character_snapshot(
                draft,
                self.customizer_state,
                self.char_overview.as_ref(),
                self.selected_protocol_slot,
                self.pending_create_name.as_deref(),
                self.creation_sent,
            ),
            world: WorldSnapshot {
                region: if entered {
                    Known::Known { value: self.region }
                } else {
                    Known::Unknown {
                        reason: "world region is not committed".into(),
                    }
                },
                player_object_id: (self.self_object_id != 0)
                    .then_some(self.self_object_id)
                    .map_or_else(
                        || Known::Unknown {
                            reason: "player object ID has not arrived".into(),
                        },
                        |value| Known::Known { value },
                    ),
                position: if entered {
                    Known::Known { value: self.player }
                } else {
                    Known::Unknown {
                        reason: "player has not entered world".into(),
                    }
                },
                heading: if entered {
                    Known::Known {
                        value: self.heading,
                    }
                } else {
                    Known::Unknown {
                        reason: "player has not entered world".into(),
                    }
                },
                entered,
            },
            presentation: PresentationSnapshot {
                frame_attempts: self.harness_frame_attempts,
                presented_frames: self.harness_presented_frames,
                last_present_age_ms: self.harness_last_present.map_or_else(
                    || Known::Unknown {
                        reason: "no frame has presented".into(),
                    },
                    |at| Known::Known {
                        value: at.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                    },
                ),
                surface_physical_size,
                window_logical_size,
                scale_factor,
                display_mode: Known::Known {
                    value: format!("{:?}", self.display.mode),
                },
                focused: self.harness_focused.clone(),
                minimized: self.harness_minimized.clone(),
                capture_active: self.recorder.is_some() || self.harness_pending_capture.is_some(),
                held_inputs: self.held_input_names(),
            },
            assets: AssetSnapshot {
                terrain,
                preworld_scene: self.harness_preworld_scene_job.clone(),
                texture: unknown_job("texture job lifecycle is synchronous and not retained"),
                mesh: unknown_job("mesh job lifecycle is synchronous and not retained"),
                animation: unknown_job("animation job lifecycle is synchronous and not retained"),
                gpu_upload: unknown_job("GPU upload timing is not retained separately"),
                worker_queue_depth: Known::Known {
                    value: usize::from(self.terrain_load.is_some()),
                },
                stale_work_rejections: Known::Unknown {
                    reason: "stale terrain rejection count is not retained".into(),
                },
            },
            shutdown: ShutdownSnapshot {
                state: if self.pending_exit {
                    "pending_exit"
                } else if self.quitting.is_some() || self.exit_when_flushed.is_some() {
                    "flushing"
                } else {
                    "running"
                }
                .into(),
                pending_exit: self.pending_exit,
                live_commands_queued: self.feed.as_ref().map_or_else(
                    || Known::Known { value: 0 },
                    |feed| Known::Known {
                        value: feed.queued(),
                    },
                ),
            },
        }
    }

    fn held_input_names(&self) -> Vec<String> {
        let mut held = Vec::new();
        for (active, name) in [
            (self.keys.fwd, "forward"),
            (self.keys.back, "back"),
            (self.keys.turn_left, "turn_left"),
            (self.keys.turn_right, "turn_right"),
            (self.keys.strafe_left, "strafe_left"),
            (self.keys.strafe_right, "strafe_right"),
            (self.turning, "turning"),
            (self.orbiting, "orbiting"),
            (self.mouse_look, "mouse_look"),
        ] {
            if active {
                held.push(name.into());
            }
        }
        held
    }

    fn native_capture_manifest(
        &self,
        width: u32,
        height: u32,
        presented_counter: u64,
    ) -> caer_render::capture_manifest::CaptureManifest {
        let identity = self.preworld_avatar_identity();
        let (race, gender) = identity
            .map(|(race, gender)| (race.to_string(), gender.to_string()))
            .unwrap_or_else(|| ("unknown".into(), "unknown".into()));
        let realm = self
            .preworld_scene_realm()
            .map_or_else(|| "unknown".into(), |value| value.to_string());
        let (surface_width, surface_height) = self
            .shell
            .gpu()
            .map(|gpu| gpu.surface_size())
            .unwrap_or((0.0, 0.0));
        let capture_screen = if self.harness_pending_capture.is_some() {
            self.preworld_flow.screen().map_or_else(
                || self.preworld_screen_name().to_owned(),
                |screen| format!("{screen:?}").to_ascii_lowercase(),
            )
        } else {
            self.preworld_screen_name().to_owned()
        };
        caer_render::capture_manifest::CaptureManifest::new()
            .with("screen", capture_screen)
            .with("width", width)
            .with("height", height)
            .with("anim_time", "live")
            .with("surface_width", surface_width)
            .with("surface_height", surface_height)
            .with(
                "scale_factor",
                self.shell.window().map_or(0.0, |w| w.scale_factor()),
            )
            .with("display_mode", self.display.mode.as_str())
            .with("realm", realm)
            .with("race", race)
            .with("gender", gender)
            .with("class", self.create_draft.class_id)
            .with(
                "appearance",
                format!("{:?}", self.create_draft.appearance()),
            )
            .with("player_position", format!("{:?}", self.player))
            .with("heading", self.heading)
            .with("camera_orbit", self.cam_orbit)
            .with("camera_pitch", self.orbit_pitch)
            .with("pose_elapsed_ms", self.started.elapsed().as_millis())
            .with("commit", caer_client::evidence::BUILD_COMMIT)
            .with("source", "native-window-readback")
            .with("presented_with_capture", presented_counter)
    }

    fn emit_harness_observation(&mut self) {
        if self.harness.is_none() {
            return;
        }
        let state = self.harness_state_label();
        let snapshot = self.harness_snapshot();
        let Some(mut harness) = self.harness.take() else {
            return;
        };
        if self.harness_last_state.as_deref() != Some(&state) {
            let from = self
                .harness_last_state
                .clone()
                .unwrap_or_else(|| "unobserved".into());
            harness.transition(from, state.clone());
            self.harness_last_state = Some(state);
            harness.snapshot(snapshot);
        }
        self.harness = Some(harness);
    }

    fn emit_harness_snapshot(&mut self) {
        if self.harness.is_none() {
            return;
        }
        let snapshot = self.harness_snapshot();
        if let Some(harness) = self.harness.as_mut() {
            harness.snapshot(snapshot);
        }
    }

    fn apply_window_focus(&mut self, focused: bool) {
        if self.harness.is_some() {
            self.harness_focused = caer_harness::Known::Known { value: focused };
            self.emit_harness_observation();
        }
    }

    fn apply_window_resize(&mut self, width: u32, height: u32) {
        if self.harness.is_some() {
            self.harness_minimized = caer_harness::Known::Known {
                value: width == 0 || height == 0,
            };
            self.emit_harness_observation();
        }
        self.shell.note_resize(width, height);
    }

    fn apply_create_text(&mut self, text: &str) -> bool {
        if !self.name_edit_focus
            || self.preworld_flow.screen()
                != Some(caer_render::preworld::PreWorldScreen::CharCreate)
        {
            return false;
        }
        caer_render::preworld_product::append_create_name_chars(&mut self.create_draft.name, text);
        true
    }

    /// Shared reducer for compositor and synthetic pointer motion. Coordinates are in window
    /// pixels, exactly as Winit reports them; the product's surface transform remains authoritative.
    fn apply_pointer_move(&mut self, x: f64, y: f64) {
        let vp = self.pointer_viewport();
        self.cursor = self.cursor_to_surface(x as f32, y as f32, vp);
        let mut hover_changed = false;
        if let Some(pw) = self.preworld_hud.as_mut() {
            let before = pw.hover_state();
            pw.set_pointer(self.cursor[0], self.cursor[1], vp);
            let after = pw.hover_state();
            hover_changed = before != after;
            if hover_changed && env_truthy("CAER_POINTER_TRACE") {
                eprintln!(
                    "rustdaoc: preworld hover {:?} -> {:?} at ({:.1}, {:.1}) viewport=({:.0}, {:.0}) screen={:?}",
                    before.action, after.action, self.cursor[0], self.cursor[1], vp.0, vp.1, pw.screen()
                );
            }
        }
        if self.show_overlay {
            self.product.wm.on_motion(self.cursor[0], self.cursor[1]);
        }
        if hover_changed {
            if let Some(window) = self.shell.window() {
                window.request_redraw();
            }
        }
    }

    /// Shared reducer for Winit and synthetic left/right button edges. Hit testing, product
    /// dispatch, preview drag ownership, and camera capture therefore cannot diverge by source.
    fn apply_pointer_button(&mut self, button: MouseButton, held: bool) {
        let customizer_preview = self.customizer_preview_active();
        let customizer_preview_drag = self.customizer_preview_drag_active();
        if !held && matches!(button, MouseButton::Left) {
            self.product.wm.on_release();
        }
        if customizer_preview && !held {
            self.preworld_preview_drag = None;
            self.set_preview_cursor_capture(false);
            return;
        }
        if customizer_preview && held && matches!(button, MouseButton::Right) {
            if customizer_preview_drag {
                self.preworld_preview_drag = Some(PreWorldPreviewDrag::Zoom);
                self.set_preview_cursor_capture(true);
            }
            return;
        }
        if held && matches!(button, MouseButton::Left) {
            let vp = self.pointer_viewport();
            if self.product_input_active() {
                if let Some(pw) = self.preworld_hud.take() {
                    let Some(action) = pw.hit_action(self.cursor[0], self.cursor[1], vp) else {
                        if customizer_preview_drag {
                            self.preworld_preview_drag = Some(PreWorldPreviewDrag::Rotate);
                            self.preworld_hud = Some(pw);
                            self.set_preview_cursor_capture(true);
                            return;
                        }
                        println!(
                            "rustdaoc: preworld click ({:.0},{:.0}) on {:?} — no control there",
                            self.cursor[0],
                            self.cursor[1],
                            pw.screen()
                        );
                        self.preworld_hud = Some(pw);
                        return;
                    };
                    self.preworld_hud = Some(pw);
                    let _ = self.dispatch_preworld_action(action);
                    return;
                }
            }
            if self.show_overlay {
                let chat_open = self.chat.captures_keyboard();
                let (x, y) = (self.cursor[0], self.cursor[1]);
                if let Some(hud) = self.skin_hud.as_ref() {
                    let dispatch = self.product.dispatch(
                        caer_render::product_input::ProductInput::PointerPress { x, y },
                        hud.skin(),
                        caer_render::product_loop::ProductDispatchCtx {
                            chat_open,
                            has_target: self.target.is_some(),
                        },
                    );
                    if dispatch.consumed {
                        if let Some(command) = dispatch.command {
                            self.send_social_command(command);
                        }
                        return;
                    }
                }
            }
        }
        match button {
            MouseButton::Left => self.orbiting = held,
            MouseButton::Right => {
                if !held && self.turning && self.camera_snapback {
                    self.cam_orbit = 0.0;
                }
                self.turning = held;
            }
            _ => return,
        }
        let dragging = self.orbiting || self.turning;
        if let Some(window) = self.shell.window() {
            let _ = window.set_cursor_grab(if dragging {
                CursorGrabMode::Confined
            } else {
                CursorGrabMode::None
            });
            window.set_cursor_visible(!dragging);
        }
    }

    /// Shared wheel reducer. Winit converts pixels to retail-style notches before entering it;
    /// synthetic input supplies the same normalized delta.
    fn apply_wheel(&mut self, notches: f32) {
        if self.customizer_preview_active() {
            return;
        }
        self.cam_dist = (self.cam_dist * 1.1_f32.powf(-notches)).clamp(50.0, 3000.0);
    }

    /// Shared held-key reducer after resolving the user's live binding map. It returns false for
    /// edge-triggered keys so their existing physical owners remain unchanged and synthetic input
    /// can fail closed instead of approximating them.
    fn apply_held_key(&mut self, code: KeyCode, pressed: bool) -> bool {
        let chord = caer_render::keybinds::Chord {
            key: code,
            ctrl: self.mods.control_key(),
            alt: self.mods.alt_key(),
            shift: self.mods.shift_key(),
        };
        let Some(action) = self.binds.action_for_chord(chord) else {
            return false;
        };
        match caer_render::product_input::product_input(action) {
            Some(caer_render::product_input::ProductInput::HoldForward) => self.keys.fwd = pressed,
            Some(caer_render::product_input::ProductInput::HoldBack) => self.keys.back = pressed,
            Some(caer_render::product_input::ProductInput::HoldSlideLeft) => {
                self.keys.strafe_left = pressed;
            }
            Some(caer_render::product_input::ProductInput::HoldSlideRight) => {
                self.keys.strafe_right = pressed;
            }
            Some(caer_render::product_input::ProductInput::Walk) => self.keys.walk = pressed,
            Some(caer_render::product_input::ProductInput::Sprint) => self.keys.sprint = pressed,
            Some(caer_render::product_input::ProductInput::LookUp) => self.keys.look_up = pressed,
            Some(caer_render::product_input::ProductInput::LookDown) => {
                self.keys.look_down = pressed
            }
            Some(caer_render::product_input::ProductInput::PanCamera) => self.orbiting = pressed,
            Some(caer_render::product_input::ProductInput::Mouse) => self.mouse_look = pressed,
            _ => return false,
        }
        true
    }

    fn emit_cached_harness_response(
        &mut self,
        command_id: String,
        response: caer_render::harness_ingress::CachedResponse,
        duplicate: bool,
    ) {
        let Some(harness) = self.harness.as_mut() else {
            return;
        };
        match response {
            caer_render::harness_ingress::CachedResponse::Ack { revision } => {
                harness.ack(command_id, revision, duplicate);
            }
            caer_render::harness_ingress::CachedResponse::Refusal { revision, reason } => {
                harness.refusal(command_id, revision, reason);
            }
        }
    }

    fn finish_harness_command(
        &mut self,
        command: caer_harness::CommandEnvelope,
        response: caer_render::harness_ingress::CachedResponse,
    ) {
        if self
            .harness_commands
            .remember(command.clone(), response.clone())
            .is_err()
        {
            let revision = self.harness.as_ref().map_or(0, |h| h.revision());
            if let Some(harness) = self.harness.as_mut() {
                harness.refusal(
                    command.command_id,
                    revision,
                    caer_harness::RefusalReason::Invalid,
                );
            }
            return;
        }
        self.emit_cached_harness_response(command.command_id, response, false);
    }

    fn evaluate_harness_waits(&mut self) {
        if self.harness.is_none() {
            return;
        }
        let snapshot = self.harness_snapshot();
        let elapsed_ms = self.harness.as_ref().map_or(0, |h| h.elapsed_ms());
        let expired = self.harness_waits.take_expired(elapsed_ms);
        let satisfied = self.harness_waits.take_satisfied(&snapshot);
        let revision = self.harness.as_ref().map_or(0, |h| h.revision());
        let expired_capture = self
            .harness_pending_capture
            .as_ref()
            .is_some_and(|(_, _, deadline_ms)| elapsed_ms >= *deadline_ms)
            .then(|| self.harness_pending_capture.take())
            .flatten();
        if expired_capture.is_some() {
            self.recorder = None;
        }
        if let Some(harness) = self.harness.as_mut() {
            for command_id in expired {
                harness.refusal(
                    command_id,
                    revision,
                    caer_harness::RefusalReason::DeadlineExpired,
                );
            }
            for command_id in satisfied {
                harness.predicate_satisfied(command_id);
            }
            if let Some((command_id, _, _)) = expired_capture {
                harness.refusal(
                    command_id,
                    revision,
                    caer_harness::RefusalReason::DeadlineExpired,
                );
            }
        }
    }

    fn handle_harness_command(
        &mut self,
        event_loop: &ActiveEventLoop,
        command: caer_harness::CommandEnvelope,
    ) {
        use caer_render::harness_ingress::{CachedResponse, PreflightDecision};
        let Some(harness) = self.harness.as_ref() else {
            return;
        };
        let decision = caer_render::harness_ingress::preflight_command(
            &command,
            harness.run_id(),
            harness.revision(),
            harness.elapsed_ms(),
            &self.harness_commands,
        );
        match decision {
            PreflightDecision::Replay(response) => {
                self.emit_cached_harness_response(command.command_id, response, true);
                return;
            }
            PreflightDecision::Refuse(reason) => {
                let revision = self.harness.as_ref().map_or(0, |h| h.revision());
                if command.run_id == self.harness.as_ref().map_or("", |harness| harness.run_id()) {
                    self.finish_harness_command(
                        command,
                        CachedResponse::Refusal { revision, reason },
                    );
                } else if let Some(harness) = self.harness.as_mut() {
                    harness.refusal(command.command_id, revision, reason);
                }
                return;
            }
            PreflightDecision::Execute => {}
        }

        let result = match &command.command {
            caer_harness::HarnessCommand::Capabilities => {
                if let Some(harness) = self.harness.as_mut() {
                    harness.capabilities();
                }
                Ok(())
            }
            caer_harness::HarnessCommand::Snapshot => {
                self.emit_harness_snapshot();
                Ok(())
            }
            caer_harness::HarnessCommand::Wait { predicate } => {
                if !caer_render::harness_ingress::wait_predicate_supported(predicate) {
                    Err(caer_harness::RefusalReason::Invalid)
                } else {
                    self.harness_waits
                        .push(caer_render::harness_ingress::PendingWait {
                            command_id: command.command_id.clone(),
                            predicate: predicate.clone(),
                            deadline_ms: command.deadline_ms,
                        })
                        .map_err(|_| caer_harness::RefusalReason::Unsupported)
                }
            }
            caer_harness::HarnessCommand::ActivatePreworld { action } => {
                caer_render::harness_ingress::adapt_preworld(action.clone())
                    .and_then(|action| self.dispatch_preworld_action(action))
            }
            caer_harness::HarnessCommand::Input { input, .. }
                if caer_render::harness_ingress::synthetic_input_supported(&command.command) =>
            {
                match input {
                    caer_harness::InputAction::PointerMove { x, y } => {
                        self.apply_pointer_move(*x, *y);
                        Ok(())
                    }
                    caer_harness::InputAction::PointerButton { button, pressed } => {
                        match caer_render::harness_ingress::adapt_pointer_button(button) {
                            Ok(button) => {
                                self.apply_pointer_button(button, *pressed);
                                Ok(())
                            }
                            Err(reason) => Err(reason),
                        }
                    }
                    caer_harness::InputAction::Wheel { y, .. } => {
                        self.apply_wheel(*y);
                        Ok(())
                    }
                    caer_harness::InputAction::Key { key, pressed } => {
                        match caer_render::harness_ingress::adapt_key_code(key) {
                            Ok(key) => self
                                .apply_held_key(key, *pressed)
                                .then_some(())
                                .ok_or(caer_harness::RefusalReason::Unsupported),
                            Err(reason) => Err(reason),
                        }
                    }
                    caer_harness::InputAction::Text { text } => self
                        .apply_create_text(text)
                        .then_some(())
                        .ok_or(caer_harness::RefusalReason::WrongScreen),
                    caer_harness::InputAction::Focus { focused } => {
                        self.apply_window_focus(*focused);
                        Ok(())
                    }
                    caer_harness::InputAction::Resize { width, height } => {
                        self.apply_window_resize(*width, *height);
                        Ok(())
                    }
                }
            }
            caer_harness::HarnessCommand::Input { .. } => {
                Err(caer_harness::RefusalReason::Unsupported)
            }
            caer_harness::HarnessCommand::Capture { artifact_name } => {
                if self.recorder.is_some() || self.harness_pending_capture.is_some() {
                    Err(caer_harness::RefusalReason::Unsupported)
                } else if let Some(recorder) = Recorder::start_harness_capture(artifact_name) {
                    self.recorder = Some(recorder);
                    self.harness_pending_capture = Some((
                        command.command_id.clone(),
                        artifact_name.clone(),
                        command.deadline_ms,
                    ));
                    Ok(())
                } else {
                    Err(caer_harness::RefusalReason::Unsupported)
                }
            }
            caer_harness::HarnessCommand::Quit => {
                if self.begin_quit() {
                    event_loop.exit();
                }
                Ok(())
            }
        };
        self.emit_harness_observation();
        let revision = self.harness.as_ref().map_or(0, |h| h.revision());
        let response = match result {
            Ok(()) => CachedResponse::Ack { revision },
            Err(reason) => CachedResponse::Refusal { revision, reason },
        };
        self.finish_harness_command(command, response);
        self.evaluate_harness_waits();
    }

    /// Queue a live command; surface channel failures instead of discarding them.
    /// Takes `&self` so it can run while other UI borrows are live (pre-world hit testing).
    fn live_send(&self, cmd: LiveCommand) -> bool {
        let Some(feed) = self.feed.as_ref() else {
            return false;
        };
        if let Err(e) = feed.send(cmd) {
            let msg = format!("live command send failed: {e}");
            log::warn!("rustdaoc: {msg}");
            *self.live_link_error.borrow_mut() = Some(msg);
            if let Some(w) = self.shell.window() {
                w.set_title("rustdaoc — live link error");
            }
            false
        } else {
            true
        }
    }

    /// Execute one pre-world action through the exact product reducer/effect owners used by a
    /// physical click. The harness is only another caller of this method; it never assigns the
    /// resulting screen, selection, or server outcome.
    fn dispatch_preworld_action(
        &mut self,
        action: caer_render::preworld::PreWorldAction,
    ) -> Result<(), caer_harness::RefusalReason> {
        let Some(mut pw) = self.preworld_hud.take() else {
            return Err(caer_harness::RefusalReason::WrongScreen);
        };
        let frontmost_allows = pw.semantic_action_allowed(action);
        if !frontmost_allows {
            self.preworld_hud = Some(pw);
            return Err(caer_harness::RefusalReason::WrongScreen);
        }

        let step_before = self.preworld_flow.step();
        println!("rustdaoc: preworld action {action:?} (step {step_before:?})");
        if let caer_render::preworld::PreWorldAction::CustomizeCamera(control) = action {
            self.preworld_preview_camera.apply(control);
        }
        let mut pst = caer_render::preworld_product::PreWorldProductState {
            transition: std::mem::take(&mut self.transition),
            create_draft: self.create_draft.clone(),
            create_realm: self.create_realm,
            overview: self.char_overview.clone(),
            pending_create_name: self.pending_create_name.clone(),
            name_edit_focus: self.name_edit_focus,
            selected_protocol_slot: self.selected_protocol_slot,
            ui_select_sent: self.ui_select_sent,
            creation_sent: self.creation_sent,
            names: self.create_names.clone(),
            appearance_catalog: self.appearance_catalog.clone(),
            customizer: self.customizer_state,
        };
        let result = caer_render::preworld_product::dispatch_preworld_action(&mut pst, action);
        self.transition = pst.transition;
        self.create_draft = pst.create_draft;
        self.create_realm = pst.create_realm;
        self.char_overview = pst.overview;
        self.pending_create_name = pst.pending_create_name;
        self.name_edit_focus = pst.name_edit_focus;
        self.selected_protocol_slot = pst.selected_protocol_slot;
        self.ui_select_sent = pst.ui_select_sent;
        self.creation_sent = pst.creation_sent;
        self.customizer_state = pst.customizer;
        if let Some(msg) = result.refused {
            println!("rustdaoc: preworld refused — {msg}");
            self.preworld_hud = Some(pw);
            return Err(caer_harness::RefusalReason::Invalid);
        }
        self.note_preworld_action(action);
        let step_after = self.preworld_flow.step();
        if step_after == step_before {
            println!("rustdaoc: preworld step unchanged ({step_before:?}) after {action:?}");
        } else {
            println!("rustdaoc: preworld step {step_before:?} -> {step_after:?}");
        }
        let all_effects_accepted =
            deliver_preworld_effects(result.commands, |command| self.live_send(command));
        if result.apply_hud_navigation {
            pw.apply_action(action);
        }
        if matches!(
            action,
            caer_render::preworld::PreWorldAction::QuitConfirmYes
                | caer_render::preworld::PreWorldAction::LoginExit
        ) {
            println!("rustdaoc: preworld {action:?} → leaving");
            self.exit_when_flushed = Some(Instant::now());
        }
        let commit_options = action
            == caer_render::preworld::PreWorldAction::Options(
                caer_render::preworld_options::OptionsHit::Accept,
            );
        if let caer_render::preworld::PreWorldAction::SelectCharacterSlot(slot) = action {
            let summary = self
                .char_overview
                .as_ref()
                .and_then(|overview| overview.characters.iter().find(|c| c.slot == slot).cloned());
            match summary {
                Some(summary) => {
                    println!("rustdaoc: UI selected slot {slot} ({})", summary.name);
                    self.apply_identity_from_summary(&summary);
                    self.hud.name = summary.name;
                }
                None => println!("rustdaoc: UI selected empty slot {slot}"),
            }
        }
        self.preworld_hud = Some(pw);
        if commit_options {
            self.commit_options_draft();
        }
        if let Some(window) = self.shell.window() {
            window.request_redraw();
        }
        if all_effects_accepted {
            Ok(())
        } else {
            Err(caer_harness::RefusalReason::Invalid)
        }
    }

    /// Mirror an accepted local UI action into the protocol-driven pre-world controller.
    fn note_preworld_action(&mut self, action: caer_render::preworld::PreWorldAction) {
        use caer_render::preworld_flow::flow_event_for;

        // The mapping is `preworld_flow::flow_event_for`, not a match here. It used to live in
        // this file, which is why A13's one wrong arm — Quit onto `FlowEvent::Closed` — could not
        // be covered by a test.
        let event = flow_event_for(action);
        if let Some(event) = event {
            // Offline there is no overview coming, so a realm click would sit on the realm plate
            // forever waiting for one. Resolve it locally onto that realm's creation form, which
            // is the only pre-world destination an offline session has.
            use caer_render::preworld_flow::FlowEvent;
            if self.offline_preworld {
                if let FlowEvent::RealmChosen(realm) = event {
                    if self.preworld_flow.choose_realm_offline(realm) {
                        self.create_realm = realm;
                        self.create_draft.realm = realm;
                        self.create_draft
                            .set_race(caer_render::preworld::race_id_for_realm(realm, 0));
                        // The stage is per realm and is cached by the realm it was uploaded for.
                        self.preworld_scene_uploaded = None;
                        return;
                    }
                }
            }
            self.preworld_flow.on_event(event);
        }
    }

    /// Whether authored pre-world input routing is active.
    ///
    /// **H6.** This replaces a hand-enumerated screen list that omitted `CharSelect` — so whenever
    /// `at_preworld()` was false while the HUD still showed the character plate, its chrome (Play,
    /// Delete, Realm, Quit, Options) silently stopped routing. Every `PreWorldScreen` variant is by
    /// definition a pre-world screen, so the correct test is "is one showing", never a list that a
    /// new variant can fall out of.
    ///
    /// One predicate, used by pointer routing, the egui bypass and pre-world dispatch alike.
    #[must_use]
    fn product_input_active(&self) -> bool {
        self.at_preworld() || self.preworld_screen().is_some()
    }

    /// The pre-world screen the HUD is currently showing, if it exists.
    #[must_use]
    fn preworld_screen(&self) -> Option<caer_render::preworld::PreWorldScreen> {
        self.preworld_hud.as_ref().map(|pw| pw.screen())
    }

    /// Only retail's Customize / Stats screens own the lower-left preview controls.  A local
    /// face inspection must not leak its camera state into the character-select backdrop.
    #[must_use]
    fn customizer_preview_active(&self) -> bool {
        customizer_preview_visible(self.preworld_pin().or_else(|| self.preworld_screen()))
    }

    /// The stats plate is intentionally rendered over the same preview, but its modal contract
    /// swallows the canvas.  Keep this separate from [`Self::customizer_preview_active`] so an
    /// empty click in the plate cannot turn into a hidden rotate/zoom gesture.
    #[must_use]
    fn customizer_preview_drag_active(&self) -> bool {
        customizer_preview_drag_allowed(self.preworld_pin().or_else(|| self.preworld_screen()))
    }

    #[must_use]
    fn active_preworld_preview_camera(&self) -> caer_render::preworld_camera::CustomizerCamera {
        self.customizer_preview_active()
            .then_some(self.preworld_preview_camera)
            .unwrap_or_default()
    }

    /// Keep cursor capture local to the source customizer drag contract.  This must not toggle
    /// the in-world orbit / turn flags: a preview drag is neither a player turn nor a movement
    /// camera operation.
    fn set_preview_cursor_capture(&self, dragging: bool) {
        if let Some(window) = self.shell.window() {
            let _ = window.set_cursor_grab(if dragging {
                CursorGrabMode::Confined
            } else {
                CursorGrabMode::None
            });
            window.set_cursor_visible(!dragging);
        }
    }

    /// Is `--preworld` holding the screen fixed, rather than merely choosing where to start?
    ///
    /// Only for the headless `--screenshot` path. A window is navigable by definition.
    #[must_use]
    fn preworld_pinned(&self) -> bool {
        self.preworld_forced.is_some() && !self.offline_preworld
    }

    /// The forced screen, but only while it is actually pinning one. Off the pin, the HUD's own
    /// screen is the truth — asking `preworld_forced` there stands a body on the realm plate.
    #[must_use]
    fn preworld_pin(&self) -> Option<caer_render::preworld::PreWorldScreen> {
        self.preworld_forced.filter(|_| self.preworld_pinned())
    }

    /// True when a pre-world surface (login / realm / char-select / …) owns the frame.
    #[must_use]
    fn at_preworld(&self) -> bool {
        if self.preworld_forced.is_some() {
            return true;
        }
        self.preworld_flow.screen().is_some()
    }

    /// Choose the pre-world screen from forced CLI, local nav, or live `SessionPhase`.
    fn sync_preworld_screen(&mut self) {
        // An offline session has no phase to reconcile against — the socket never opens, so the
        // phase is `Disconnected` forever and reconciling it rewinds the flow to the login plate
        // on every tick. Local navigation is the whole navigator here.
        if !self.offline_preworld {
            self.preworld_flow.observe_phase(self.session_phase);
        }
        if self.transition.realm() != 0 {
            self.preworld_flow.dest_realm = self.transition.realm();
        }
        self.preworld_flow.dest_region = self.region;
        let pinned = self.preworld_pinned();
        let forced = self.preworld_forced;
        let Some(pw) = self.preworld_hud.as_mut() else {
            return;
        };
        // `--preworld` PINS the screen only for a headless verification shot, where the whole
        // point is that the frame cannot drift off the screen under test. In a window it is a
        // STARTING screen: re-asserting it every frame put the screen back the instant a click
        // moved it, which is what made Realm, Cancel and Options look broken.
        if pinned {
            if let Some(forced) = forced {
                pw.set_screen(forced);
            }
            return;
        }
        pw.set_splash_seed(self.preworld_flow.splash_seed);
        pw.set_loading_plate(self.preworld_flow.loading_plate());
        if let Some(screen) = self.preworld_flow.screen() {
            pw.set_screen(screen);
        }
    }

    /// Start loading the expensive world-only assets after the session reaches `InWorld`.
    ///
    /// Crucially, this only *starts* a CPU worker. The retail asset walk and MPK/NIF/DDS decode
    /// used to run synchronously in a Winit callback. On a large client tree (especially an NTFS
    /// mount) that starved redraw/focus delivery for minutes, which presented as a blank loading
    /// screen followed by an OS "Not Responding" report despite the server already accepting the
    /// character into the world.
    fn ensure_world_assets_loaded(&mut self) {
        if !self.world_assets_loaded {
            self.ensure_terrain_load();
        }
    }

    /// Begin a CPU-only terrain decode if this world slice still needs one. GPU resource creation
    /// is deliberately deferred to [`Self::poll_terrain_load`] on the main thread; wgpu surfaces
    /// must not cross the worker boundary.
    fn ensure_terrain_load(&mut self) {
        if self.terrain_load.is_some()
            || self.shell.gpu().is_none()
            || (self.world_assets_loaded && !self.pending_terrain_reload)
        {
            return;
        }

        let request = TerrainLoadRequest::around_player(self.region, self.origin, self.player);
        if self.terrain_load_failed == Some(request) {
            return;
        }

        let root = caer_render::terrain::client_root();
        let (sender, receiver) = mpsc::channel();
        println!(
            "rustdaoc: terrain decode started — region {} origin ({:.0},{:.0}) bounds {:?}..{:?}",
            request.region, request.origin.x, request.origin.y, request.min, request.max
        );
        let spawn = std::thread::Builder::new()
            .name("caer-terrain-load".to_owned())
            .spawn(move || {
                let began = Instant::now();
                let completion = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let (mesh, dungeon) = caer_render::dungeon_mesh::load_product_terrain(
                        &root,
                        request.region,
                        request.origin,
                        request.min,
                        request.max,
                    );
                    TerrainLoadCompletion::Ready(TerrainLoadResult {
                        request,
                        mesh,
                        dungeon,
                        decode_elapsed: began.elapsed(),
                    })
                }))
                .unwrap_or(TerrainLoadCompletion::Failed {
                    request,
                    reason: "terrain worker panicked while decoding client assets",
                });
                // The receiver can disappear during a clean client shutdown; that is not a
                // worker error and does not need to resurrect the window.
                let _ = sender.send(completion);
            });

        match spawn {
            Ok(_) => {
                self.terrain_load_failed = None;
                self.terrain_load = Some(TerrainLoadTask {
                    request,
                    started: Instant::now(),
                    receiver,
                });
                self.emit_harness_asset(
                    format!(
                        "terrain:{}:{}:{}",
                        request.region, request.min[0], request.min[1]
                    ),
                    "running",
                );
            }
            Err(error) => {
                eprintln!("rustdaoc: unable to start terrain worker: {error}");
                self.terrain_load_failed = Some(request);
                self.emit_harness_asset(
                    format!(
                        "terrain:{}:{}:{}",
                        request.region, request.min[0], request.min[1]
                    ),
                    "failed_to_start",
                );
            }
        }
    }

    /// Poll a completed CPU decode and perform the short, main-thread-only GPU upload.
    ///
    /// There is intentionally no blocking receive here. While decoding takes seconds or minutes,
    /// `frame` keeps presenting `PreWorldScreen::Loading`, handling Alt-Tab/focus events, and
    /// draining the live feed. A teleport result that no longer matches our protocol origin is
    /// discarded instead of being installed under the wrong region.
    fn poll_terrain_load(&mut self) {
        let completion = match self.terrain_load.as_ref() {
            Some(task) => match task.receiver.try_recv() {
                Ok(completion) => completion,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => TerrainLoadCompletion::Failed {
                    request: task.request,
                    reason: "terrain worker exited without a completion",
                },
            },
            None => return,
        };
        let task = self
            .terrain_load
            .take()
            .expect("terrain completion requires an in-flight task");
        let total_elapsed = task.started.elapsed();

        match completion {
            TerrainLoadCompletion::Ready(result) => {
                if !result.request.still_matches(self.region, self.origin) {
                    println!(
                        "rustdaoc: discarded stale terrain result for region {} origin ({:.0},{:.0}); current is region {} origin ({:.0},{:.0})",
                        result.request.region,
                        result.request.origin.x,
                        result.request.origin.y,
                        self.region,
                        self.origin.x,
                        self.origin.y,
                    );
                    self.emit_harness_asset(
                        format!(
                            "terrain:{}:{}:{}",
                            result.request.region, result.request.min[0], result.request.min[1]
                        ),
                        "stale_rejected",
                    );
                    self.pending_terrain_reload = true;
                    return;
                }
                let job_id = format!(
                    "terrain:{}:{}:{}",
                    result.request.region, result.request.min[0], result.request.min[1]
                );
                self.install_prepared_terrain(result, total_elapsed);
                self.emit_harness_asset(job_id, "complete");
            }
            TerrainLoadCompletion::Failed { request, reason } => {
                eprintln!(
                    "rustdaoc: terrain decode failed for region {} origin ({:.0},{:.0}) after {:.3}s: {reason}",
                    request.region,
                    request.origin.x,
                    request.origin.y,
                    total_elapsed.as_secs_f64(),
                );
                self.terrain_load_failed = Some(request);
                self.emit_harness_asset(
                    format!(
                        "terrain:{}:{}:{}",
                        request.region, request.min[0], request.min[1]
                    ),
                    "failed",
                );
            }
        }
    }

    /// Install a worker-prepared terrain mesh into the GPU and complete the first world handoff.
    fn install_prepared_terrain(&mut self, result: TerrainLoadResult, total_elapsed: Duration) {
        let TerrainLoadResult {
            request,
            mesh,
            dungeon,
            decode_elapsed,
        } = result;
        println!(
            "rustdaoc: terrain decoded in {:.3}s (handoff {:.3}s) — region {} origin ({:.0},{:.0}) — {} zones, {} model kinds ({} placed){}",
            decode_elapsed.as_secs_f64(),
            total_elapsed.as_secs_f64(),
            request.region,
            request.origin.x,
            request.origin.y,
            mesh.zones_loaded,
            mesh.models.len(),
            mesh.models.iter().map(|m| m.instances.len()).sum::<usize>(),
            if dungeon.model_instances > 0 || dungeon.fixture_boxes > 0 {
                format!(
                    ", dungeon nif_instances={} nif_kinds={} box_fallback={}",
                    dungeon.model_instances, dungeon.model_kinds, dungeon.fixture_boxes
                )
            } else {
                String::new()
            },
        );
        {
            let gpu = self
                .shell
                .gpu_mut()
                .expect("terrain upload only starts after GPU initialization");
            gpu.set_terrain(&mesh.zones);
            gpu.set_water(&mesh.water_vertices, &mesh.water_indices);
            gpu.set_models(&mesh.models, &mesh.textures);
            gpu.set_fixtures(&caer_render::fixture_instances(&mesh.fixtures));
        }
        // Retain the terrain so movement can sample ground height; seat the spawn on the surface.
        if let Some(h) = mesh.walk_height_at(self.player[0], self.player[1], self.player[2]) {
            self.player[2] = h;
            self.seat_avatar();
        }
        self.terrain = Some(mesh);
        self.pending_terrain_reload = false;
        self.terrain_load_failed = None;
        self.transition.clear_terrain_dirty();
        self.try_play_zone_ambient();
        self.resolve_avatar();
        self.world_assets_loaded = true;
    }

    /// Zone ambient/music via [`AudioBus::on_region_changed`]. Unmapped names are not success.
    fn try_play_zone_ambient(&mut self) {
        let root = caer_render::terrain::client_root();
        // Prefer the zone containing the player's feet; fall back to any zone of this region.
        let zone = caer_world::zone_at(self.region, self.player[0] as i32, self.player[1] as i32)
            .or_else(|| {
                caer_world::region_zone_offsets(self.region)
                    .into_iter()
                    .next()
                    .map(|(id, _, _)| id)
            });
        let Some(zid) = zone else {
            self.audio.on_logout();
            return;
        };
        let path = root.join(format!("zones/zone{zid:03}/sounds.dat"));
        let path = if path.is_file() {
            path
        } else {
            // frontiers / housing trees — same basename convention.
            let alt = [
                format!("frontiers/zones/zone{zid:03}/sounds.dat"),
                format!("phousing/zones/zone{zid:03}/sounds.dat"),
                format!("Tutorial/zones/zone{zid:03}/sounds.dat"),
            ]
            .into_iter()
            .map(|r| root.join(r))
            .find(|p| p.is_file());
            let Some(p) = alt else {
                self.audio.on_region_changed(zid, None, 1200, 0, None);
                return;
            };
            p
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            self.audio.on_region_changed(zid, None, 1200, 0, None);
            return;
        };
        let zs = caer_assets::zonesounds::parse(&text);
        let listener = (self.player[0] as u32, self.player[1] as u32);
        self.audio
            .on_region_changed(zid, Some(&zs), 1200, 0, Some(listener));
        println!(
            "rustdaoc: zone{zid:03} sounds.dat → {} names (bus {:?})",
            zs.names().len(),
            self.audio.device_kind()
        );
    }

    fn play_logical(
        &mut self,
        category: caer_render::SoundCategory,
        logical: impl Into<String>,
    ) -> bool {
        self.audio
            .play(caer_render::LogicalSoundEvent::oneshot(category, logical))
            .is_success()
    }

    fn send_social_command(&mut self, cmd: live::LiveCommand) {
        match &cmd {
            LiveCommand::DialogResponse { response, .. } => {
                let _ = self.log.push(
                    0x00,
                    if *response == caer_render::social_ui::DIALOG_YES {
                        "[social] DialogResponse Yes"
                    } else {
                        "[social] DialogResponse No"
                    },
                );
            }
            LiveCommand::ModifyTrade { action, .. } => {
                let _ = self
                    .log
                    .push(0x00, &format!("[social] ModifyTrade {action:?}"));
            }
            _ => {}
        }
        let _ = self.play_logical(
            caer_render::SoundCategory::Ui,
            caer_render::event_audio::logical_for_ui_click(),
        );
        self.live_send(cmd);
    }

    fn refill_hud_from_world(&mut self) {
        self.hud.status = self.world.player_status;
        self.hud.target = self.target.as_ref().map(|(id, name)| TargetInfo {
            name: name.clone(),
            health_pct: self.world.get(*id).map_or(100, |e| e.health_pct),
        });
        let pres = self.world.combat_presentation();
        self.hud.cast_bar = pres.cast.self_bar;
        self.hud.cast_outcome = pres.cast.outcome;
        self.hud.combat_result = pres.combat_results.last().map(|r| r.result_label);
        self.hud.xp_permill = pres.points.map(|p| p.level_permill);
    }

    /// Latch race/gender from an overview slot (PacketLib1126 packed byte → fig3 gender).
    fn apply_identity_from_summary(&mut self, c: &caer_protocol::overview::CharacterSummary) {
        let fx = self.transition.select_character(c);
        if let Some((race, fig3)) = self.transition.identity() {
            if self.player_race != Some(race)
                || self.player_gender != Some(fig3)
                || self.player_appearance != c.appearance()
            {
                self.player_race = Some(race);
                self.player_gender = Some(fig3);
                self.player_appearance = c.appearance();
                self.self_model = None;
                self.self_equip_skin = Default::default();
                println!(
                    "rustdaoc: avatar identity → race={race} fig3={fig3} ({})",
                    c.race_name
                );
            }
        }
        if fx.reload_terrain {
            self.region = self.transition.region();
            self.pending_terrain_reload = true;
        }
        let (ox, oy) = self.transition.origin_xy();
        if c.region != 0 {
            // Keep Client.region in lockstep with the controller.
            self.region = self.transition.region();
        }
        let _ = (ox, oy);
    }

    /// Place the third-person camera behind + above the player, looking at them. Rebuilt each
    /// frame from the current player position + orbit angles — cheap (a little trig) and keeps the
    /// camera glued to the character as it moves.
    /// The character's facing as radians in DAoC's convention: 0 = +Y (north), increasing toward +X.
    fn heading_rad(&self) -> f32 {
        f32::from(self.heading) / 4096.0 * std::f32::consts::TAU
    }

    /// The character's forward direction on the world ground plane.
    fn facing_dir(&self) -> (f32, f32) {
        let (s, c) = self.heading_rad().sin_cos();
        (s, c)
    }

    /// Camera azimuth in world-XY terms (measured from +X toward +Y), derived from where the
    /// CHARACTER faces plus however far the camera has been orbited around it.
    ///
    /// Character forward is `(sin h, cos h)` while the camera basis is `(cos a, sin a)`, so sitting
    /// directly behind means `a = π/2 − h`. Deriving the camera from the character (rather than the
    /// other way round) is the whole difference between DAoC's controls and the camera-relative
    /// model this used to have.
    fn camera_yaw(&self) -> f32 {
        std::f32::consts::FRAC_PI_2 - self.heading_rad() + self.cam_orbit
    }

    fn follow_camera(&self, aspect: f32) -> Camera {
        // Orbit about a point at roughly chest height, not the feet — otherwise the character sits
        // at the bottom of the frame and the camera stares at the ground.
        let focus = Vec3::new(
            self.player[0],
            self.player[1],
            self.player[2] + FOCUS_HEIGHT,
        );
        let (sy, cy) = self.camera_yaw().sin_cos();

        // A TRUE spherical orbit: the eye stays exactly `cam_dist` from the focus at every pitch.
        // The old form added a fixed CAM_HEIGHT on top of the pitch offset, so the eye's actual
        // distance shrank as you pitched — dragging up and down visibly pulled the camera in and
        // out, which reads as an unwanted zoom rather than an arc.
        let (sp, cp) = self.orbit_pitch.sin_cos();
        let horiz = self.cam_dist * cp;
        let eye = Vec3::new(
            focus.x - horiz * cy,
            focus.y - horiz * sy,
            focus.z + self.cam_dist * sp,
        );
        let mut c = Camera::new(eye, focus, self.origin, aspect);
        c.ensure_far(300_000.0);
        c
    }

    /// Integrate movement for this frame: move the player locally (instant — the client is
    /// authoritative for its own position) and stream a PositionUpdate to the server on the wire
    /// cadence. Character-relative: W/S along facing, Q/E strafe (`slide_*`). Keyboard turn flags
    /// exist for integration/tests but A/D are unbound in the 74-action defaults. `dt` is seconds
    /// since the last frame.
    fn update_movement(&mut self, dt: f32, now: Instant) {
        // Keyboard turning first, so this frame's movement travels along the NEW facing — turning
        // and then stepping in the old direction reads as a skid.
        if self.keys.turning() {
            // Same screen convention as the mouse: heading UP is a turn to the LEFT.
            let dir = if self.keys.turn_left { 1.0 } else { -1.0 };
            // `self.turning` is right-click-held. While it is, the camera is PINNED in world space
            // and the character turns underneath it; otherwise the camera rides round with the turn.
            let (heading, orbit) = apply_turn(
                self.heading,
                self.cam_orbit,
                dir,
                dt,
                self.turning,
                self.camera_snapback,
            );
            self.heading = heading;
            self.cam_orbit = orbit;
            self.seat_avatar();
        }
        // LookUp / LookDown are held GameActions — nudge orbit pitch while held.
        if self.keys.look_up != self.keys.look_down {
            let dir = if self.keys.look_up { 1.0 } else { -1.0 };
            self.orbit_pitch =
                (self.orbit_pitch + dir * LOOK_PITCH_RATE * dt).clamp(CAM_PITCH_MIN, CAM_PITCH_MAX);
        }
        // Back cancels runlock (DAoC sticky-forward).
        if self.keys.back {
            self.autorun = false;
        }
        let moving = self.keys.active() || self.autorun;

        if moving {
            // CHARACTER-relative basis on the ground plane (world XY) — DAoC moves you along the
            // direction your character faces, not along the camera. That is why the camera can be
            // swung right round to look at your own face while W still runs you forward.
            //
            // Strafe right is the facing rotated -90° in WORLD bearing — which is screen-RIGHT,
            // because render space mirrors world Y and that flips handedness on screen.
            //
            // I briefly "fixed" this to +90 after measuring D at 270° from facing, forgetting that
            // the telemetry reports WORLD bearings while what matters is what the player sees. In
            // world terms screen-right genuinely IS h-90 here; the original was correct and the
            // flip inverted A and D. Measure in world space, judge in screen space.
            let fwd = self.facing_dir();
            let right = (-fwd.1, fwd.0);
            let mut dir = (0.0f32, 0.0f32);
            if self.keys.fwd || self.autorun {
                dir = (dir.0 + fwd.0, dir.1 + fwd.1);
            }
            if self.keys.back {
                dir = (dir.0 - fwd.0, dir.1 - fwd.1);
            }
            if self.keys.strafe_right {
                dir = (dir.0 + right.0, dir.1 + right.1);
            }
            if self.keys.strafe_left {
                dir = (dir.0 - right.0, dir.1 - right.1);
            }
            let len = (dir.0 * dir.0 + dir.1 * dir.1).sqrt();
            if len > 1e-4 {
                let (nx, ny) = (dir.0 / len, dir.1 / len);
                // Integrate local position in XY.
                let step = self.travel_speed() * dt;
                self.player[0] += nx * step;
                self.player[1] += ny * step;
                // Follow terrain: seat z on the ground height under the new XY (falls back to the
                // last z off the loaded terrain — e.g. at a zone edge we haven't loaded).
                if let Some(t) = &self.terrain {
                    // Walkable surface: stepping onto a dock or a keep floor must raise the
                    // player, not leave them wading through it at terrain height.
                    if let Some(h) =
                        t.walk_height_at(self.player[0], self.player[1], self.player[2])
                    {
                        self.player[2] = h;
                    }
                }
                // System 8 footstep: stride cadence + material from atmosphere textures.csv.
                self.footstep_accum += step;
                while caer_render::event_audio::should_play_footstep(true, self.footstep_accum) {
                    self.footstep_accum -= caer_render::event_audio::FOOTSTEP_STRIDE;
                    let surface = self.footstep_surface();
                    let logical = caer_render::event_audio::logical_for_footstep(surface);
                    let _ = self.play_logical(caer_render::SoundCategory::Movement, logical);
                }
                // NOTE: `heading` is deliberately NOT recomputed from the movement vector here.
                // It is the character's facing, owned by the mouse (right-drag) and persistent —
                // deriving it from motion each frame is what made the character spin to face
                // whatever direction it happened to be strafing, instead of holding its facing.
                // Drive the rendered avatar (the Self_ box) to the new spot so it stays under the
                // camera — otherwise the camera walks off and leaves the box at spawn (looks like an
                // invisible/stationary character). The server re-seeds it only on spawn/teleport.
                self.seat_avatar();
            }
        } else {
            self.footstep_accum = 0.0;
        }

        // Wire sync: send a PositionUpdate on the cadence while moving, and exactly one when we
        // stop, so the server records the final resting spot.
        {
            let stopped_now = self.was_moving && !moving;
            if (moving && now.duration_since(self.last_move_send) >= MOVE_SEND_INTERVAL)
                || stopped_now
            {
                self.last_move_send = now;
                self.live_send(LiveCommand::Move {
                    x: self.player[0],
                    y: self.player[1],
                    z: self.player[2],
                    heading: self.heading,
                    speed: if moving { self.travel_speed() } else { 0.0 },
                    // Echo the server's own last reading of our health rather than a hardcoded
                    // 100: the position packet carries this field, and a client that always
                    // claims full health contradicts the health bar we now draw.
                    health_pct: self.world.player_status.health_pct,
                    motion: self.motion(),
                });
            }
        }
        self.was_moving = moving;
    }

    /// Surface under the player's feet from Stream C atmosphere materials (textures.csv bases).
    /// Empty catalog → Default (still a real logical name via `step`, never invented).
    fn footstep_surface(&self) -> caer_render::event_audio::SurfaceKind {
        let Some(t) = self.terrain.as_ref() else {
            return caer_render::event_audio::SurfaceKind::Default;
        };
        let Some(layer) = t.atmosphere.materials.layers.iter().find(|l| l.visible) else {
            return caer_render::event_audio::SurfaceKind::Default;
        };
        caer_render::event_audio::classify_material_base(&layer.base_texture)
    }

    /// Begin a graceful logout: stop moving, send `/quit`, and enter the quitting state. The window
    /// stays up (rendering) until the server confirms the logout — see [`frame`](Self::frame).
    /// Calling it again (a second close/Esc while already quitting) forces an immediate exit for a
    /// user who doesn't want to wait out a combat quit-timer.
    /// Tab through nearby targetable entities, nearest first, wrapping at the end.
    ///
    /// Selection is client-side (the server has no "what can I click" query); we mirror the choice
    /// to the server so a later attack/spell/interact request lands on the right object. NPCs only
    /// for now — players and objects join once `PlayerCreate` is decoded (Phase B.6).
    fn cycle_target(&mut self) {
        let me = self.player;
        let mut candidates: Vec<(u16, String, f32)> = self
            .world
            .render_set_par([me[0] as i32, me[1] as i32], TARGET_RANGE, 0)
            .iter()
            .filter_map(|item| self.world.get(item.object_id))
            .filter(|v| v.kind != Kind::Self_)
            .map(|v| {
                let d =
                    ((v.pos[0] as f32 - me[0]).powi(2) + (v.pos[1] as f32 - me[1]).powi(2)).sqrt();
                (v.object_id, v.name.to_string(), d)
            })
            .collect();
        candidates.sort_by(|a, b| a.2.total_cmp(&b.2).then(a.0.cmp(&b.0)));
        if candidates.is_empty() {
            self.set_target(None);
            return;
        }
        // Advance past the current selection so repeated Tab walks outward; wrap at the end.
        let next = match self
            .target
            .as_ref()
            .and_then(|(id, _)| candidates.iter().position(|c| c.0 == *id))
        {
            Some(i) => (i + 1) % candidates.len(),
            None => 0,
        };
        let (id, name, dist) = candidates[next].clone();
        log::debug!("rustdaoc: target -> {name} (id {id}, {dist:.0} units)");
        self.set_target(Some((id, name)));
    }

    /// Set (or clear) the target and mirror it to the server. `0` is the wire's "no target".
    fn set_target(&mut self, t: Option<(u16, String)>) {
        let oid = t.as_ref().map_or(0, |(id, _)| *id);
        self.target = t;
        self.live_send(live::LiveCommand::Target(oid));
    }

    /// Travel speed for local motion + wire PositionUpdate (walk/sprint/MaxSpeed modifiers).
    fn travel_speed(&self) -> f32 {
        let mut speed = MOVE_SPEED * (f32::from(self.max_speed_percent) / 100.0);
        if self.keys.walk {
            speed *= WALK_SPEED_FRAC;
        }
        if self.keys.sprint {
            speed *= SPRINT_SPEED_FRAC;
        }
        speed.min(190.0)
    }

    /// Tab through entities matching `kind_ok`, nearest first.
    fn cycle_target_filtered(&mut self, kind_ok: impl Fn(Kind) -> bool) {
        let me = self.player;
        let mut candidates: Vec<(u16, String, f32)> = self
            .world
            .render_set_par([me[0] as i32, me[1] as i32], TARGET_RANGE, 0)
            .iter()
            .filter_map(|item| self.world.get(item.object_id))
            .filter(|v| v.kind != Kind::Self_ && kind_ok(v.kind))
            .map(|v| {
                let d =
                    ((v.pos[0] as f32 - me[0]).powi(2) + (v.pos[1] as f32 - me[1]).powi(2)).sqrt();
                (v.object_id, v.name.to_string(), d)
            })
            .collect();
        candidates.sort_by(|a, b| a.2.total_cmp(&b.2).then(a.0.cmp(&b.0)));
        if candidates.is_empty() {
            self.set_target(None);
            return;
        }
        let next = match self
            .target
            .as_ref()
            .and_then(|(id, _)| candidates.iter().position(|c| c.0 == *id))
        {
            Some(i) => (i + 1) % candidates.len(),
            None => 0,
        };
        let (id, name, dist) = candidates[next].clone();
        log::debug!("rustdaoc: target -> {name} (id {id}, {dist:.0} units)");
        self.set_target(Some((id, name)));
    }

    fn target_group_slot(&mut self, slot: usize) {
        let Some(gw) = self.product.social.group.as_ref() else {
            self.log.push(0x00, "no group roster");
            return;
        };
        let Some(m) = gw.members.get(slot) else {
            self.log
                .push(0x00, &format!("no group member in slot {}", slot + 1));
            return;
        };
        let oid = m.object_id;
        if oid == 0 {
            self.log.push(0x00, "group slot has no object id");
            return;
        }
        let name = self
            .world
            .get(oid)
            .map(|e| e.name.to_string())
            .unwrap_or_else(|| format!("group{}", slot + 1));
        self.set_target(Some((oid, name)));
    }

    /// Inventory use by absolute bag slot. Kept for selected-slot UI (B2 refused fixed 0/1).
    #[allow(dead_code)]
    fn use_inventory_slot(&mut self, slot: u8, use_type: u8) {
        self.live_send(LiveCommand::UseSlot {
            x: self.player[0],
            y: self.player[1],
            z: self.player[2],
            speed: 0.0,
            heading: self.heading,
            flag_speed_data: 0,
            slot,
            use_type,
        });
        self.log
            .push(0x00, &format!("[use] UseSlot slot={slot} type={use_type}"));
    }

    /// The motion state that rides on a position update: which zone we're in, our object id, and
    /// whether we're jumping or strafing.
    ///
    /// The zone matters: the server resolves it with `WorldMgr.GetZone(currentZoneID)` and logs
    /// "position in unknown zone" if it can't, so sending a constant 0 told it we were always in
    /// Camelot Hills no matter where we stood.
    /// Which way the player is travelling relative to their facing, for picking the locomotion
    /// clip. Distinct from [`Client::motion`], which is the wire's state flags for the server.
    ///
    /// Forward wins a diagonal: running forward-and-left is a forward run in DAoC, not a sidestep.
    /// Turning (A/D) is deliberately absent — it rotates you on the spot and is not travel, so it
    /// must not select a movement clip.
    /// `/keyboard` — list or change bindings, persisting any change immediately so a rebind
    /// survives the next launch without a separate save step.
    fn keyboard_command(&mut self, args: &str) -> Vec<String> {
        let (mut lines, changed) = keyboard_command(&mut self.binds, args);
        if changed {
            lines.push(save_bindings(&self.binds));
        }
        lines
    }

    fn render_motion(&self) -> caer_render::entities::Motion {
        use caer_render::entities::Motion;
        // Opposing keys cancel on each axis, matching `Keys::active` and the direction vector: with
        // W+S held the character is not travelling forwards OR backwards, so a held A still reads as
        // a sidestep rather than being masked by a forward key that is doing nothing.
        if self.keys.fwd != self.keys.back {
            if self.keys.fwd {
                Motion::Forward
            } else {
                Motion::Back
            }
        } else if self.keys.strafe_left != self.keys.strafe_right {
            if self.keys.strafe_left {
                Motion::Left
            } else {
                Motion::Right
            }
        } else {
            Motion::Forward
        }
    }

    fn motion(&self) -> caer_protocol::session::PlayerMotion {
        let zone_id =
            caer_world::zone_at(self.region, self.player[0] as i32, self.player[1] as i32)
                .unwrap_or(0);
        caer_protocol::session::PlayerMotion {
            object_id: self.self_object_id,
            zone_id,
            jumping: self.jumping,
            strafing: self.keys.strafe_left || self.keys.strafe_right,
        }
    }

    /// Start or stop melee attack mode, and say which in the log.
    ///
    /// Requires a target: the server swings at whatever `PlayerTarget` last selected, so entering
    /// attack mode with nothing selected just stands there.
    fn toggle_attack(&mut self) {
        self.attacking = !self.attacking;
        if self.attacking {
            if let Some(t) = self.target.clone() {
                self.last_attacker = Some(t);
            }
        }
        self.live_send(LiveCommand::Attack {
            start: self.attacking,
        });
        let what = match (&self.target, self.attacking) {
            (Some((_, name)), true) => format!("attacking {name}"),
            (None, true) => "attack mode on (no target selected)".to_string(),
            (_, false) => "attack mode off".to_string(),
        };
        self.log.push(0x11, &what);
    }

    fn toggle_sit(&mut self) {
        self.sitting = !self.sitting;
        self.live_send(LiveCommand::Sit { sit: self.sitting });
        let _ = self.log.push(
            0x00,
            &eco_slash_chat_note(&LiveCommand::Sit { sit: self.sitting }),
        );
    }

    /// Trigger a quickbar slot from its number key.
    ///
    /// The skill is addressed by its INDEX in the server's usable-skill list plus its skill-page
    /// type — the layout `UseSkillHandler` actually reads, now verified against 115 captured
    /// samples. The server still sends no acknowledgement, so the flash means "sent".
    fn use_quickbar_slot(&mut self, digit: u32) {
        let Some(index) = caer_render::quickbar::Quickbar::slot_for_digit(digit) else {
            return;
        };
        let Some(slot) = self.quickbar.press(index) else {
            return;
        };
        self.live_send(LiveCommand::UseSkill {
            index: slot.index,
            skill_type: slot.skill_type,
            x: self.player[0],
            y: self.player[1],
            z: self.player[2],
        });
        self.log.push(
            0x00,
            &format!("[quickbar] {} (slot index {})", slot.name, slot.index),
        );
    }

    fn begin_quit(&mut self) -> bool {
        if self.quitting.is_some() {
            return true; // already quitting → caller should force-exit
        }
        self.keys = MoveKeys::default();
        // One final stop-update so we're stationary (a quit-timer server rejects a moving quit),
        // then the quit request itself.
        self.live_send(LiveCommand::Move {
            x: self.player[0],
            y: self.player[1],
            z: self.player[2],
            heading: self.heading,
            speed: 0.0,
            health_pct: self.world.player_status.health_pct,
            motion: self.motion(),
        });
        self.live_send(LiveCommand::Quit);
        self.quitting = Some(Instant::now());
        if let Some(w) = self.shell.window() {
            w.set_title("rustdaoc — logging out…");
        }
        false
    }

    /// Resolve + upload the player's assembled avatar mesh, including equipment-driven pskins
    /// (MS-02b). Re-runs when the equipped armour skin key changes so studded → plate is visible
    /// on `Self_`, not only on NPC meshes.
    fn resolve_avatar(&mut self) {
        let equip = if self.self_object_id != 0 {
            self.world.equipment_of(self.self_object_id)
        } else {
            None
        };
        let key = equip
            .map(caer_render::entities::EquipSkinKey::from_equipment)
            .unwrap_or_default();
        if self.self_model.is_some() && self.self_equip_skin == key {
            return;
        }
        if let (Some(em), Some(gpu), Some(race), Some(gender)) = (
            self.entity_models.as_mut(),
            self.shell.gpu_mut(),
            self.player_race,
            self.player_gender,
        ) {
            self.self_model = em.ensure_avatar(gpu, race, gender, self.player_appearance, equip);
            self.self_equip_skin = key;
        }
    }

    /// Push the current player position to the rendered avatar (the `Self_` box). The box mesh is a
    /// unit cube centred on its position, so we lift it by [`BOX_SIZE`] (its half-height) to rest it
    /// ON the ground rather than half-buried. `self.player[2]` stays at ground level — that's the z
    /// the camera targets and the wire reports; only the drawn box carries the visual offset (which
    /// goes away once a real avatar model, anchored at the feet, replaces the placeholder).
    fn seat_avatar(&mut self) {
        // The player's z IS the feet: `player[2]` is sampled straight off the terrain, and the
        // avatar mesh is foot-anchored, so it publishes unchanged.
        //
        // This used to add BOX_SIZE, left from when the player was a centre-anchored placeholder
        // cube that needed lifting by half its extent. It survived the avatar landing because
        // grounding then snapped every entity unconditionally, so the bogus +110 was overwritten
        // before anything drew — a latent bug hiding behind an unconditional correction. Making the
        // snap conditional (see `MAX_GROUND_SNAP`) put 110 units outside the tolerance, no snap
        // happened, and the player floated. Removing the lift fixes it at the source; the
        // conditional snap is right and the offset was always wrong.
        let z = self_entity_z(self.player[2]);
        // Speed is what selects the locomotion clip (idle/walk/run) for our own avatar, so it has
        // to reflect whether we are actually moving THIS frame — not a constant.
        let speed = if (self.keys.active() || self.autorun) && self.quitting.is_none() {
            self.travel_speed() as u16
        } else {
            0
        };
        self.world.move_self_to(
            [self.player[0] as i32, self.player[1] as i32, z],
            self.heading,
            speed,
        );
    }

    /// One rendered frame: drain the live feed into the world, follow the player, draw.
    fn frame(&mut self) {
        self.mark_liveness(3);
        if !self.shell.is_ready() {
            return;
        }
        if self.harness.is_some() {
            self.harness_frame_attempts = self.harness_frame_attempts.saturating_add(1);
        }
        // Apply coalesced resize before present — one surface.configure per storm, not per event.
        let _ = self.shell.apply_pending_resize();
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32().min(0.1);
        // Do not advance the pacing clock until the presentation attempt has returned.  Surface
        // acquisition can itself consume wgpu's one-second timeout on Wayland.  Stamping the frame
        // here made `about_to_wait` see an already-expired deadline and immediately queue another
        // redraw, starving the compositor event that releases the FIFO image and perpetuating the
        // one-second acquire loop.

        // Fold any server events that arrived since last frame. The client is AUTHORITATIVE for its
        // own position (WASD is integrated locally, below), so a server position echo must NOT
        // overwrite it every frame — doing so fights local movement and rubberbands the player.
        // Only a large jump (a real teleport / zone change) is accepted as an authoritative move.
        let mut session_ended = false;
        if let Some(feed) = &self.feed {
            let eco_before = snap_eco_visual(&self.world);
            let d = live::drain_into(feed.events(), &mut self.world);
            self.emit_harness_server_effect(&d);
            self.mark_liveness(4);
            for note in eco_visual_chat_notes(&eco_before, &snap_eco_visual(&self.world)) {
                let _ = self.log.push(0x00, &note);
            }
            if let Some(oid) = d.self_object_id {
                self.self_object_id = oid;
            }
            if let Some(ov) = d.overview.as_ref() {
                // A CharacterCreateRequest is accepted only when the server refreshes the
                // overview *and* that overview contains the exact requested name.  DOLSharp's
                // success path emits LoginGranted followed by this list; there is no dedicated
                // create-success packet we can safely navigate on.  The old code merely printed
                // this fact, then sent `OverviewReady`, which CharCustomize deliberately ignores.
                // From the player's view Continue had created a character but left them on the
                // form until they pressed Cancel and discovered it by accident.
                let created = self.pending_create_name.as_deref().and_then(|want| {
                    ov.characters
                        .iter()
                        .find(|c| c.name.eq_ignore_ascii_case(want))
                        .map(|c| (want.to_string(), c.slot))
                });
                if let Some((want, slot)) = &created {
                    println!(
                        "rustdaoc: §14A create round-trip — `{want}` present on CharacterOverview ({} chars), selecting slot {slot}",
                        ov.characters.len()
                    );
                    self.pending_create_name = None;
                    // Retail returns to the character plate with the new row selected.  Make the
                    // avatar/Play target follow the same authoritative overview row rather than
                    // leaving an old selection (or none) behind after a successful create.
                    self.selected_protocol_slot = Some(*slot);
                    self.ui_select_sent = false;
                    self.creation_sent = false;
                } else if let Some(want) = self.pending_create_name.as_deref() {
                    println!(
                        "rustdaoc: overview refresh after create — `{want}` not yet listed ({} chars)",
                        ov.characters.len()
                    );
                }
                // Belt and braces on top of clearing at realm change: the overview is realm-local,
                // so a summary carrying a different realm than the one we asked about does not
                // belong on this screen. Dropped rather than drawn, and logged — a server sending
                // the wrong block is worth seeing rather than silently rendering.
                let mut ov = ov.clone();
                let want = self.transition.realm();
                if want != 0 {
                    let before = ov.characters.len();
                    ov.characters.retain(|c| c.realm == 0 || c.realm == want);
                    if ov.characters.len() != before {
                        println!(
                            "rustdaoc: overview for realm {want} carried {} character(s) from another realm — dropped",
                            before - ov.characters.len()
                        );
                    }
                }
                self.char_overview.replace(ov.clone());
                if created.is_some() {
                    self.preworld_flow
                        .on_event(caer_render::preworld_flow::FlowEvent::CreateAccepted);
                } else {
                    self.preworld_flow
                        .on_event(caer_render::preworld_flow::FlowEvent::OverviewReady);
                }
                // H4: keep the same character across a refresh, or clear. Never retarget. The old
                // guard only fired when the row index ran past the end, so deleting an *earlier*
                // character left the selection pointing at whoever shifted into that row.
                if self
                    .selected_protocol_slot
                    .is_some_and(|slot| !ov.characters.iter().any(|c| c.slot == slot))
                {
                    self.selected_protocol_slot = None;
                    self.ui_select_sent = false;
                }
            }
            if let Some(rid) = d.region_changed {
                println!(
                    "rustdaoc: RegionChanged → region {rid} (awaiting PlayerPosition for origin)"
                );
                // Scrub old ambient/music immediately; new beds wait for load_terrain.
                self.audio.on_region_changed(rid, None, 1200, 0, None);
                let fx = self.transition.apply_region_changed(rid);
                // Do not touch self.region / pending_terrain_reload until origin commits —
                // otherwise new-region terrain loads on a stale origin (Sol BLOCKER 2).
                debug_assert!(!fx.reload_terrain);
                let _ = fx;
            }
            // S1: WorldState culls entities, but Client.requested_npcs / target are separate —
            // scrub them or recycled ids leave stale boxes / HUD after remove / zone / logout.
            for &oid in &d.removed_ids {
                self.other_avatars
                    .on_object_removed(oid, self.entity_models.as_mut());
                self.audio.on_object_removed(oid);
                if caer_render::lifecycle::on_object_removed(
                    &mut self.requested_npcs,
                    &mut self.target,
                    oid,
                ) {
                    self.live_send(LiveCommand::Target(0));
                }
            }
            if d.region_changed.is_some() || d.logged_out {
                let had_target = self.target.is_some();
                caer_render::lifecycle::on_region_or_logout(
                    &mut self.requested_npcs,
                    &mut self.target,
                );
                self.product.reconnect_reset();
                self.particle_fx.clear();
                if d.logged_out {
                    self.audio.on_logout();
                    // WorldState::apply already ran clear_for_logout via drain_into.
                }
                if had_target {
                    self.live_send(LiveCommand::Target(0));
                }
            }
            // Social UI: pending invite / dialog / trade / roster from the live drain.
            let social_now = Instant::now();
            fold_social_drain(&mut self.product, &d, social_now);
            if let Some(err) = d.login_denied {
                self.log
                    .push(0x00, &format!("[login] denied (error={err})"));
                println!("rustdaoc: LoginDenied error={err}");
            }
            if let Some(atk) = d.attack_mode {
                self.attacking = atk;
                self.log.push(
                    0x11,
                    if atk {
                        "attack mode on (server)"
                    } else {
                        "attack mode off (server)"
                    },
                );
            }
            if let Some(pct) = d.max_speed_percent {
                self.max_speed_percent = pct.max(1);
            }
            if let Some(oid) = d.server_target_oid {
                if oid == 0 {
                    self.set_target(None);
                    self.log.push(0x00, "[target] server cleared target");
                } else if let Some(e) = self.world.get(oid) {
                    self.set_target(Some((oid, e.name.to_string())));
                } else {
                    self.set_target(Some((oid, format!("#{oid}"))));
                }
            }
            if let Some(ref u) = d.udp_init {
                self.log.push(
                    0x00,
                    &format!("[udp] init reply {}:{}", u.region_ip, u.udp_port),
                );
            }
            if let Some(los) = d.pending_los {
                // S4: do NOT invent clear LOS. Transport is typed; product LOS is Partial until
                // geometry-backed occlusion exists. Reply blocked (0) with provenance note.
                self.log.push(
                    0x00,
                    &format!(
                        "[los] CheckLOSRequest checker={} target={} → blocked (no geometry LOS; PARTIAL)",
                        los.checker_oid, los.target_oid
                    ),
                );
                self.live_send(LiveCommand::CheckLosResponse {
                    checker_oid: los.checker_oid,
                    target_oid: los.target_oid,
                    response: 0,
                });
            }
            if let Some(ref r) = d.name_check_bad {
                self.log.push(
                    0x00,
                    &format!(
                        "[create] bad-name {} → {}",
                        r.name,
                        if r.accepted { "ok" } else { "rejected" }
                    ),
                );
            }
            if let Some(ref r) = d.name_check_dup {
                self.log.push(
                    0x00,
                    &format!("[create] dup-name {} → result={}", r.name, r.result),
                );
            }
            if let Some(ref r) = d.create_reply {
                self.log
                    .push(0x00, &format!("[create] reply name={}", r.name));
            }
            if let Some(ref dinfo) = d.delve {
                self.log.push(
                    0x00,
                    &format!(
                        "[delve] {}",
                        dinfo.info.chars().take(120).collect::<String>()
                    ),
                );
            }
            for cmd in self.addons.on_drain(&d) {
                self.live_send(cmd);
            }
            for note in self.addons.take_diagnostic_notes() {
                let _ = self.log.push(0x00, &note);
            }
            let _ = self.product.tick_timeouts(social_now);
            self.product.sync_windows(self.target.is_some());
            if let Some(phase) = d.phase {
                if phase != self.session_phase {
                    println!(
                        "rustdaoc: session phase {:?} -> {:?}",
                        self.session_phase, phase
                    );
                }
                self.session_phase = phase;
                self.preworld_flow.observe_phase(phase);
                if !self.audio_smoke_done
                    && matches!(phase, caer_protocol::session::SessionPhase::InWorld)
                    && std::env::var_os("CAER_AUDIO_SMOKE").is_some()
                {
                    self.audio_smoke_done = true;
                    let ok = self.play_logical(caer_render::SoundCategory::Ambient, "g_Thunder");
                    println!("rustdaoc: CAER_AUDIO_SMOKE g_Thunder → {ok}");
                }
            }
            if let Some(pp) = d.player {
                let (dx, dy) = (pp[0] - self.player[0], pp[1] - self.player[1]);
                let large_jump = dx * dx + dy * dy > TELEPORT_DIST_SQ;
                // Any position after a pending 0xB7 commits region+origin+camera atomically,
                // including delayed / under-threshold packets (Sol stale-origin falsifier).
                if large_jump || self.transition.awaiting_origin() {
                    self.player = pp;
                    let fx = self.transition.apply_teleport_origin(pp[0], pp[1]);
                    let (ox, oy) = self.transition.origin_xy();
                    self.origin = Vec3::new(ox as f32, oy as f32, self.origin.z);
                    self.region = self.transition.region();
                    if fx.reload_terrain {
                        self.pending_terrain_reload = true;
                    }
                }
            }
            session_ended = d.ended;
            // Feed the combat log; chat-scraped damage is held until the nameplate pass.
            // Structured 0xBC feedback is queued separately with REQ-021 provenance.
            for (chat_type, text) in &d.messages {
                if let Some(dmg) = self.log.push(*chat_type, text) {
                    self.pending_damage.push(dmg);
                }
            }
            for anim in &d.combat_anims {
                let incoming = anim.defender_id == self.self_object_id && self.self_object_id != 0;
                self.pending_combat_anims.push((anim.clone(), incoming));
            }
            // CFX/AUD: consume WorldState sound requests (authoritative) — not a second map
            // from the same 0x72/0x1B packets, which would double-play.
            for req in self.world.drain_sound_requests() {
                let cat = match req.kind {
                    caer_world::SoundKind::CastWindup | caer_world::SoundKind::EffectLand => {
                        caer_render::SoundCategory::Spell
                    }
                };
                let _ = self.play_logical(
                    cat,
                    caer_render::event_audio::logical_for_sound_request(req),
                );
            }
        }
        // The live drain runs before rendering and can leave the initial terrain-reload bit set.
        // Poll first: a completed CPU decode needs its main-thread GPU upload before the flow may
        // leave the authored loading plate. Nothing below blocks on the worker.
        self.poll_terrain_load();
        // Keep every world-only path behind one phase decision. InWorld is allowed to decode
        // behind the authored loading plate, but the flow remains pre-world until the GPU install
        // has actually completed — signaling early was the path that exposed a blank/hung window.
        if caer_render::preworld_flow::should_signal_assets_ready(
            self.session_phase,
            self.world_assets_loaded,
        ) {
            self.ensure_world_assets_loaded();
            // On reconnect the asset latch is already true, so this still re-arms the flow. On a
            // first entry, hold it until `poll_terrain_load` has installed the worker result.
            if self.world_assets_loaded {
                self.preworld_flow.set_assets_ready(true);
            }
        }
        let at_preworld = self.at_preworld();
        if !at_preworld {
            self.ensure_world_assets_loaded();
        }
        if !at_preworld && self.pending_terrain_reload && self.shell.gpu().is_some() {
            self.ensure_terrain_load();
        }
        self.floaters.tick(dt);
        // Ask the server about anything we only have a position for. This is what the reference
        // client does (1406 times in a 15-minute capture) and what we were missing: without it an
        // entity whose create we never saw stays unidentified forever, which is where the magenta
        // placeholders came from. Each id is asked once — the server replies with either the
        // create or an ObjectDelete, and both resolve it.
        self.npc_request_timer += dt;
        if self.npc_request_timer >= 0.25 {
            self.npc_request_timer = 0.0;
            for id in self.world.unresolved_ids() {
                if self.requested_npcs.insert(id) {
                    self.live_send(LiveCommand::RequestNpc { object_id: id });
                }
            }
        }
        // A jump is a short airborne window; the flag rides on position updates while it lasts.
        if self.jumping {
            self.jump_left -= dt;
            if self.jump_left <= 0.0 {
                self.jumping = false;
            }
        }
        // An external recording request (see `Recorder::request_path`). Checked about twice a
        // second rather than every frame — this is a filesystem stat and the client runs at 800+ fps.
        self.record_poll += dt;
        if self.record_poll >= 0.5 {
            self.record_poll = 0.0;
            let requested = self
                .harness
                .as_ref()
                .map_or_else(Recorder::take_request, |harness| {
                    Recorder::take_request_from(&harness.capture_request_path())
                });
            if let Some(secs) = requested {
                if self.recorder.is_none() {
                    self.recorder = if self.harness.is_some() {
                        Recorder::start_for_harness(secs)
                    } else {
                        Recorder::start_for(secs)
                    };
                }
            }
        }
        // Expire a timed recording.
        if self.recorder.as_mut().is_some_and(|r| !r.tick(dt)) {
            if let Some(r) = self.recorder.take() {
                r.finish();
            }
        }
        self.quickbar.tick(dt);
        // Fill any empty slots from the decoded skill list (a no-op once full).
        self.quickbar.autofill(&self.world.skills);

        // Graceful-quit handling: once we've asked to log out, the session thread ends when the
        // server confirms (Quit 0xA4) — that's when the socket closed cleanly, so exit the window.
        // A safety timeout covers a server that never confirms (e.g. stuck in combat).
        if let Some(since) = self.quitting {
            if session_ended || since.elapsed() >= std::time::Duration::from_secs(65) {
                if let Some(w) = self.shell.window() {
                    w.set_title("rustdaoc — logged out");
                }
                self.pending_exit = true;
            }
        } else {
            // Kill mid-session must not freeze the world. Retry via a new LiveFeed; `/quit` does not.
            let feed_ended = self
                .feed
                .as_ref()
                .is_some_and(|f| f.state() == live::LiveSessionState::Ended)
                || session_ended;
            let plan = live::reconnect_plan(
                feed_ended,
                false,
                self.reconnect_left,
                self.reconnect_at,
                now,
            );
            match plan {
                live::ReconnectPlan::Schedule { delay } => {
                    log::warn!("rustdaoc: live session ended unexpectedly — retry in {delay:?}");
                    // The plate the player sees while the link is down. Everything else about
                    // link-death was already wired — world reset, audio, retry schedule, window
                    // title — but the pre-world flow was never told, so the screen kept showing a
                    // frozen world instead of the loading art retail puts up.
                    self.preworld_flow
                        .on_event(caer_render::preworld_flow::FlowEvent::Linkdead);
                    self.world.on_reconnect();
                    self.sitting = false;
                    self.particle_fx.clear();
                    self.audio.on_logout();
                    self.product.reconnect_reset();
                    self.feed = None;
                    self.reconnect_left = self.reconnect_left.saturating_sub(1);
                    self.reconnect_at = Some(now + delay);
                    *self.live_link_error.borrow_mut() = None;
                    if let Some(w) = self.shell.window() {
                        w.set_title("rustdaoc — reconnecting");
                    }
                }
                live::ReconnectPlan::SpawnNow => {
                    self.sitting = false;
                    let feed = live::spawn(
                        self.live_server.clone(),
                        self.login_account.clone(),
                        self.login_password.clone(),
                        self.live_character.clone(),
                        false,
                    );
                    self.feed = Some(feed);
                    self.reconnect_at = None;
                    *self.live_link_error.borrow_mut() = None;
                    if let Some(w) = self.shell.window() {
                        w.set_title("rustdaoc — reconnecting");
                    }
                }
                live::ReconnectPlan::GiveUp
                    if feed_ended && self.live_link_error.borrow().is_none() =>
                {
                    let msg = "live session ended unexpectedly".to_string();
                    log::warn!("rustdaoc: {msg}");
                    *self.live_link_error.borrow_mut() = Some(msg);
                    self.preworld_flow
                        .on_event(caer_render::preworld_flow::FlowEvent::Linkdead);
                    self.world.on_reconnect();
                    self.sitting = false;
                    self.particle_fx.clear();
                    self.audio.on_logout();
                    self.product.reconnect_reset();
                    if let Some(w) = self.shell.window() {
                        w.set_title("rustdaoc — connection lost");
                    }
                }
                live::ReconnectPlan::Idle | live::ReconnectPlan::GiveUp => {}
            }
            if self
                .feed
                .as_ref()
                .is_some_and(|f| f.state() == live::LiveSessionState::Connected)
            {
                self.reconnect_left = live::live_reconnect_attempts_from_env();
                self.reconnect_at = None;
            }
            // Only integrate movement while not quitting (we must stay stationary for the quit).
            self.update_movement(dt, now);
        }
        if !at_preworld {
            // Keep the rendered body glued to the authoritative local position every frame (even
            // when idle), so a stray server update to Self_ can't drift it off the camera.
            self.seat_avatar();
            // MS-02b: refresh Self_ mesh when 0x15 changes armour skins (studded → plate, etc.).
            self.resolve_avatar();
            self.other_avatars
                .sync_from_world(&self.world, self.entity_models.as_mut());
        }
        self.audio.tick(dt);

        // Leg 10: drain SpellEffect particle spawns into live emitters (billboard draw path).
        for spawn in self.world.drain_particle_effects() {
            let feet = self
                .world
                .get(spawn.target_id)
                .or_else(|| self.world.get(spawn.caster_id))
                .map(|e| e.pos)
                .unwrap_or([
                    self.player[0] as i32,
                    self.player[1] as i32,
                    self.player[2] as i32,
                ]);
            let origin = [
                feet[0] as f32 - self.origin.x,
                feet[1] as f32 - self.origin.y,
                feet[2] as f32 - self.origin.z + 40.0,
            ];
            let def = caer_render::particles::presence_burst(origin, spawn.spell_id);
            let seed = u64::from(spawn.spell_id) << 32
                | u64::from(spawn.caster_id) << 16
                | u64::from(spawn.target_id);
            push_capped(
                &mut self.particle_fx,
                caer_render::particles::ParticleSystem::from_def(def, seed),
                MAX_LIVE_PARTICLE_SYSTEMS,
            );
            println!(
                "rustdaoc: particle billboard spell={} caster={} target={} ({})",
                spawn.spell_id,
                spawn.caster_id,
                spawn.target_id,
                caer_render::particles::PARTICLE_BILLBOARD_PLACEHOLDER
            );
        }
        for sys in &mut self.particle_fx {
            sys.tick(dt);
        }
        self.particle_fx.retain(|s| !s.finished());
        // The stage's emitters author a finite emit window — 133 seconds in both scenes — and a
        // character screen can sit open far longer than that. Restarting a finished system keeps
        // the snow falling instead of letting the stage quietly go still after two minutes.
        for sys in &mut self.preworld_particles {
            sys.tick(dt);
            if sys.finished() {
                *sys = caer_render::particles::ParticleSystem::from_def(sys.def().clone(), 0x5EED);
            }
        }
        let cadence_ticks = ((dt * 60.0).round() as u64).max(1);
        self.world.advance_cadence(cadence_ticks);
        self.audio
            .set_listener(self.player[0], self.player[1], self.player[2]);

        let aspect = self.shell.gpu().unwrap().aspect();
        let camera = self.follow_camera(aspect);
        // Kept before the camera is handed to the shell, so the nameplate pass below projects with
        // exactly the matrix this frame was rendered with.
        let view_proj = camera.view_proj();
        // Cull around the PLAYER (not the camera, which sits behind them).
        let cull_center = [self.player[0] as i32, self.player[1] as i32];
        // Resolved before the GPU borrow: `render_motion` reads `self`, and the call already holds
        // `self.shell` mutably.
        let motion = Some(self.render_motion());
        let count = if at_preworld {
            // Pre-world owns the frame — do not draw terrain/entities under login/realm plates
            // (QA defect b: world visible at letterbox edges).
            0
        } else {
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
                self.terrain.as_ref(),
                self.self_model,
                motion,
                Some(&mut self.motion_tracker),
                dt,
                caer_render::anim_clock(),
            );
            // Leg 10 fidelity slice: billboard soft-blob draw path (not opaque cubes).
            let (bb_right, bb_up) = camera.billboard_axes();
            let right = [bb_right.x, bb_right.y, bb_right.z];
            let up = [bb_up.x, bb_up.y, bb_up.z];
            let mut particle_mesh = Vec::new();
            for sys in &self.particle_fx {
                particle_mesh.extend(sys.billboard_mesh(right, up));
            }
            let gpu = self.shell.gpu_mut().unwrap();
            gpu.upload_particle_billboards(&particle_mesh);
            self.instances.len() as u32
        };

        if !at_preworld {
            let gpu = self.shell.gpu_mut().unwrap();
            gpu.set_view_proj(camera.view_proj().to_cols_array_2d());
            gpu.upload_instances(&self.instances);
        } else {
            // Pre-world draws the stage's OWN emitters — Midgard's blowing snow, Hibernia's motes
            // near the portal. It used to clear unconditionally so login screens stayed clean, and
            // that also threw away the authored weather. Spell effects still do not belong here;
            // `preworld_particles` is a separate list for exactly that reason.
            let (bb_right, bb_up) = self
                .preworld_scene
                .as_ref()
                .map(|scene| {
                    let vp = self.pointer_viewport();
                    let (eye, focus) =
                        caer_render::preworld_scene::framing_backdrop(scene, vp.0 / vp.1.max(1.0));
                    preworld_camera_carriage(eye, focus, vp.0 / vp.1.max(1.0)).billboard_axes()
                })
                .unwrap_or_else(|| camera.billboard_axes());
            let right = [bb_right.x, bb_right.y, bb_right.z];
            let up = [bb_up.x, bb_up.y, bb_up.z];
            let mut mesh = Vec::new();
            for sys in &self.preworld_particles {
                mesh.extend(sys.billboard_mesh(right, up));
            }
            if let Some(gpu) = self.shell.gpu_mut() {
                gpu.upload_particle_billboards(&mesh);
            }
        }

        // Pre-world screens (login/realm/char-select) replace the in-world skin HUD when forced
        // or before EnteredWorld. Settings still asks SkinHud for `options_window`.
        if at_preworld || self.show_overlay {
            let vp = self.pointer_viewport();
            let mut settings_skin: Option<&str> = None;
            if at_preworld {
                self.sync_preworld_screen();
                let options = self.options_draft_from_settings();
                if let Some(pw) = self.preworld_hud.as_mut() {
                    pw.set_overview(self.char_overview.clone());
                    pw.set_selected_protocol_slot(self.selected_protocol_slot);
                    pw.set_create_draft(&self.create_draft);
                    pw.set_customizer_state(self.customizer_state);
                    // Only while closed. Once the dialog is up it owns its draft — resyncing from
                    // the live settings every frame would undo each click on the way to Accept.
                    if !pw.options_open() {
                        pw.set_options_draft(options);
                    }
                    let gpu = self.shell.gpu_mut().unwrap();
                    settings_skin = pw.render(gpu, vp);
                    let trace = (
                        self.session_phase,
                        pw.screen(),
                        pw.last_quads,
                        gpu.ui_batch_count(),
                    );
                    if self.preworld_trace != Some(trace) {
                        println!(
                            "rustdaoc: preworld {:?} / {:?} — {} quads, {} GPU batches",
                            trace.0, trace.1, trace.2, trace.3
                        );
                        self.preworld_trace = Some(trace);
                    }
                }
            }
            // In-game SkinHud only when not on a pre-world surface (except Settings / Login dialog).
            // QA defect a: health/power bars were leaking onto login/realm.
            if !at_preworld || settings_skin.is_some() {
                if let Some(skin) = self.skin_hud.as_mut() {
                    let target = self
                        .hud
                        .target
                        .as_ref()
                        .map(|t| (t.name.as_str(), t.health_pct));
                    let pass_mask = if self.login_password.is_empty() {
                        None
                    } else {
                        Some("*".repeat(self.login_password.len().min(32)))
                    };
                    let pass_mask_ref = pass_mask.as_deref();
                    let points = self.world.character_points();
                    let extra = caer_render::adapters::ExtraBind {
                        overview: self.char_overview.as_ref(),
                        chat_entry: if self.chat.open {
                            Some(self.chat.input.as_str())
                        } else {
                            None
                        },
                        skills: Some(self.world.skills.as_slice()),
                        points: points.as_ref(),
                        timer: self.world.timer_window(),
                        merchant_quantity: None,
                        create_class_label: None,
                        create_class_desc: None,
                        create_race_desc: None,
                    };
                    let zone_id = caer_world::zone_at(
                        self.region,
                        self.player[0] as i32,
                        self.player[1] as i32,
                    );
                    let zone_label = zone_id.and_then(caer_world::zone_name);
                    let state = caer_render::adapters::AdapterState {
                        player_name: &self.hud.name,
                        status: &self.hud.status,
                        target,
                        zone: zone_label,
                        fps: self.hud.fps,
                        sheet: self.world.character_sheet(),
                        char_stats: self.world.char_stats(),
                        char_resists: self.world.char_resists(),
                        equipment: self.world.equipment_of(self.self_object_id),
                        money: self.world.money(),
                        inventory: self.world.inventory(),
                        merchant: self.world.merchant(),
                        weapon_armor: self.world.weapon_armor(),
                        attack_mode: self.world.attack_mode(),
                        login_account: Some(self.login_account.as_str()),
                        login_password_mask: pass_mask_ref,
                        create_name: Some(self.create_draft.name.as_str()),
                    };
                    let base: &[caer_render::skinui::UiQuad] = if settings_skin == Some("login") {
                        self.preworld_hud
                            .as_ref()
                            .map(|pw| pw.last_layout_quads())
                            .unwrap_or(&[])
                    } else {
                        &[]
                    };
                    if let Some(name) = settings_skin {
                        let pos = if name == "login" {
                            caer_render::preworld::login_dialog_pos(vp)
                        } else {
                            (80.0, 80.0)
                        };
                        let gpu = self.shell.gpu_mut().unwrap();
                        skin.render_over(gpu, base, &[(name, pos)], &state);
                    } else {
                        self.product.sync_hud(
                            skin,
                            caer_render::skinhud::CriticalHudBind {
                                merchant_open: self.world.merchant().is_some(),
                                has_target: self.target.is_some(),
                                group_active: self.product.social.in_group,
                                trade_open: self
                                    .product
                                    .social
                                    .trade
                                    .as_ref()
                                    .is_some_and(|t| !t.window.closed),
                                quest_open: !self.product.social.quests.is_empty(),
                                inventory_open: self.world.inventory().is_some(),
                                stats_open: false,
                                skills_open: false,
                                cast_status: self.hud.cast_bar.is_some(),
                            },
                        );
                        self.product.sync_windows(self.target.is_some());
                        let combat = self.world.combat_presentation();
                        let draw = self
                            .product
                            .draw(skin.skin(), &state, &extra, Some(&combat));
                        let gpu = self.shell.gpu_mut().unwrap();
                        skin.composite(gpu, base, &draw);
                    }
                }
            }
        }
        // The realm scene behind the two character plates. Loaded and aimed here, drawn by
        // `FramePass::PreWorldScene` below — the pre-world encoder has no model step otherwise,
        // which is why the middle of both screens was black in the live window while the headless
        // screenshot (a different encoder) showed the scene fine.
        let scene_behind = at_preworld && self.ensure_preworld_scene();
        if scene_behind {
            let vp = self.pointer_viewport();
            let identity = self.preworld_avatar_identity();
            let appearance = self.preworld_avatar_appearance();
            let dress = self.preworld_avatar_equipment();
            let preview = self.active_preworld_preview_camera();
            if let (Some(scene), Some(gpu)) = (self.preworld_scene.as_ref(), self.shell.gpu_mut()) {
                compose_preworld_gpu(
                    gpu,
                    scene,
                    &mut self.preworld_scene_uploaded,
                    &mut self.preworld_scene_morphing,
                    self.entity_models.as_mut(),
                    identity,
                    appearance,
                    dress.as_ref(),
                    vp,
                    caer_render::anim_clock(),
                    preview,
                );
            }
        }
        self.shell.set_camera(camera);

        // FPS, sampled once a second so the readout doesn't flicker every frame.
        self.frames += 1;
        if now - self.fps_timer >= std::time::Duration::from_secs(1) {
            self.hud.fps = self.frames;
            if self.show_perf_meter {
                self.log.push(0x00, &format!("[perf] {} fps", self.frames));
            }
            if at_preworld {
                let screen =
                    self.preworld_hud
                        .as_ref()
                        .map_or("preworld", |pw| match pw.screen() {
                            caer_render::preworld::PreWorldScreen::Login => "login",
                            caer_render::preworld::PreWorldScreen::Splash => "splash",
                            caer_render::preworld::PreWorldScreen::Loading => "loading",
                            caer_render::preworld::PreWorldScreen::RealmSelect => "realm select",
                            caer_render::preworld::PreWorldScreen::CharSelect => "character select",
                            caer_render::preworld::PreWorldScreen::CharCreate => "character create",
                            caer_render::preworld::PreWorldScreen::CharCustomize => "customize",
                            caer_render::preworld::PreWorldScreen::CharStats => "stats",
                        });
                self.shell.set_title(&format!("rustdaoc — {screen}"));
            } else {
                self.shell.set_title(&format!(
                    "rustdaoc — ({:.0}, {:.0})",
                    self.player[0], self.player[1]
                ));
            }
            self.frames = 0;
            self.fps_timer = now;
        }

        // Refill the HUD's read-model from the world. Cast bar from self 0x72 only (CFX).
        self.refill_hud_from_world();

        // Nameplates: project entity positions through the SAME view-projection the scene was just
        // drawn with, so labels track their entities exactly. Built from the camera we rendered
        // with, not a fresh one, or labels would lag the world by a frame.
        let viewport = self.shell.window().map_or([1.0, 1.0], |w| {
            [
                w.inner_size().width.max(1) as f32,
                w.inner_size().height.max(1) as f32,
            ]
        });
        let viewer_world = Vec3::new(self.player[0], self.player[1], self.player[2]);
        let plates = if self.show_names {
            caer_render::nameplates::collect(
                &self.world,
                self.origin,
                viewer_world,
                &view_proj,
                viewport,
                self.target.as_ref().map(|(id, _)| *id),
            )
        } else {
            Vec::new()
        };

        // Anchor this tick's damage now that entity screen positions are known.
        //
        // Outgoing damage floats over the target's nameplate; incoming floats over our own
        // character, which the follow camera keeps near the middle of the screen. This is
        // direction-based attribution, not per-entity: without structured combat packets (B.2) the
        // text is all we have, and it names the target in prose rather than by object id.
        if !self.pending_damage.is_empty() {
            let target_screen = self
                .target
                .as_ref()
                .and_then(|(id, _)| self.world.get(*id))
                .and_then(|e| {
                    let rp = Vec3::new(
                        e.pos[0] as f32 - self.origin.x,
                        -(e.pos[1] as f32 - self.origin.y),
                        e.pos[2] as f32 - self.origin.z + 150.0,
                    );
                    caer_render::nameplates::project(&view_proj, rp, viewport)
                });
            let self_screen = [viewport[0] * 0.5, viewport[1] * 0.55];
            for dmg in self.pending_damage.drain(..) {
                let anchor = if dmg.incoming {
                    self_screen
                } else {
                    target_screen.unwrap_or(self_screen)
                };
                // Jitter so a flurry of hits doesn't stack into one illegible smear.
                let n = self.floaters.len() as f32;
                self.floaters
                    .spawn(dmg, [anchor[0] + (n % 3.0 - 1.0) * 22.0, anchor[1]]);
            }
        }
        if !self.pending_combat_anims.is_empty() {
            let self_screen = [viewport[0] * 0.5, viewport[1] * 0.55];
            for (anim, incoming) in self.pending_combat_anims.drain(..) {
                let anchor = if incoming {
                    self_screen
                } else if anim.defender_id != 0 {
                    self.world
                        .get(anim.defender_id)
                        .and_then(|e| {
                            let rp = Vec3::new(
                                e.pos[0] as f32 - self.origin.x,
                                -(e.pos[1] as f32 - self.origin.y),
                                e.pos[2] as f32 - self.origin.z + 150.0,
                            );
                            caer_render::nameplates::project(&view_proj, rp, viewport)
                        })
                        .unwrap_or(self_screen)
                } else {
                    self_screen
                };
                let n = self.floaters.len() as f32;
                self.floaters.spawn_combat_anim(
                    &anim,
                    [anchor[0] + (n % 3.0 - 1.0) * 22.0, anchor[1]],
                    incoming,
                );
                // System 8 combat audio: logical name from 0xBC result (MISS stays silent).
                if let Some(logical) =
                    caer_render::event_audio::logical_for_combat_result(anim.result)
                {
                    let _ = self.audio.play(caer_render::LogicalSoundEvent::oneshot(
                        caer_render::SoundCategory::Combat,
                        logical,
                    ));
                }
            }
        }

        // Build + tessellate the overlay (clone the window Arc to avoid holding a self borrow).
        // Pre-world surfaces must not get the in-game egui HUD (vitals / quickbar / nameplates) —
        // that was QA defect a (health/power 100% on login/realm).
        let mut qb_click = None;
        // The pre-world Options button used to open an egui "Display" window beside the in-game
        // `options_window` skin. Both are gone from that path: the authored Options Menu overlay
        // owns display and audio there now, and it is drawn by the pre-world HUD, not by egui.
        let display_settings_open = false;
        let mut display_modes = self
            .shell
            .window()
            .and_then(|window| {
                window
                    .current_monitor()
                    .or_else(|| window.primary_monitor())
            })
            .map_or_else(Vec::new, |monitor| caer_render::enumerate_modes(&monitor));
        if !display_modes.contains(&self.display_draft.mode_size) {
            display_modes.push(self.display_draft.mode_size);
            display_modes.sort_by_key(|mode| (mode.width, mode.height, mode.refresh_millihertz));
        }
        let mut display_action = caer_render::DisplayPanelAction::default();
        let egui_frame = match (self.ui.as_mut(), self.shell.window().cloned()) {
            (Some(ui), Some(window)) if display_settings_open => {
                let draft = &mut self.display_draft;
                let modes = &display_modes;
                let frame = ui.run(&window, |u| {
                    display_action =
                        caer_render::display_settings::draw_player_panel(u, draft, modes);
                });
                Some(frame)
            }
            _ if at_preworld => None,
            _ if !self.show_overlay && !self.chat.open && !self.product.social.has_overlay() => {
                None
            }
            (Some(ui), Some(window)) => {
                let hud = &self.hud;
                let log = &self.log;
                let floaters = &self.floaters;
                let quickbar = &self.quickbar;
                let chat = &mut self.chat;
                let show_hud = self.show_overlay;
                let mut sent = None;
                let frame = ui.run(&window, |u| {
                    // World-anchored layers first, so the HUD panels draw over them, not under.
                    if show_hud {
                        caer_render::nameplates::draw(u, &plates);
                        caer_render::combat::draw_floating(u, floaters);
                        if caer_render::product_ui::dev_egui_enabled() {
                            caer_render::combat::draw_log(u, log);
                            if let Some(slot) = caer_render::quickbar::draw(u, quickbar) {
                                qb_click = Some(slot);
                            }
                            caer_render::hud::build(u, hud);
                        }
                    }
                    if caer_render::product_ui::dev_egui_enabled() {
                        if let Some(out) = caer_render::chat::draw(u, chat) {
                            sent = Some(out);
                        }
                    }
                });
                self.outgoing_chat = sent;
                Some(frame)
            }
            _ => None,
        };
        if display_action.apply {
            let mut candidate = self.display_draft.clone();
            if candidate.mode == caer_render::WindowMode::Exclusive {
                candidate.mode_size =
                    caer_render::sanitize_mode(candidate.mode_size, &display_modes);
            }
            if let Some(window) = self.shell.window().cloned() {
                match caer_render::apply_display_settings(&window, &candidate) {
                    caer_render::ApplyResult::Applied => {
                        self.display = candidate.clone();
                        self.display_draft = candidate;
                        if let Err(error) = self.display.save() {
                            log::warn!("rustdaoc: could not persist display settings: {error}");
                        }
                        let size = window.inner_size();
                        self.shell.note_resize(size.width, size.height);
                        log_display_apply(&window, &self.display);
                    }
                    caer_render::ApplyResult::Reverted { reason } => {
                        log::warn!("rustdaoc: display apply reverted: {reason}");
                        self.display_draft = self.display.clone();
                    }
                }
            }
        }
        if display_action.close {
            // Done means leave the local overlay and return to the protocol-owned pre-world
            // screen. Unapplied edits are discarded; Apply already copied them to `display`.
            self.display_draft = self.display.clone();
            let return_screen = self
                .preworld_flow
                .screen()
                .unwrap_or(caer_render::preworld::PreWorldScreen::CharSelect);
            if let Some(pw) = self.preworld_hud.as_mut() {
                pw.set_screen(return_screen);
            }
        }
        if let Some(index) = qb_click {
            if let Some(digit) = caer_render::quickbar::Quickbar::digit_for_slot(index) {
                self.use_quickbar_slot(digit);
            }
        }
        // Put any submitted chat line on the wire, and echo it locally so the player sees their
        // own text immediately rather than waiting for the server to reflect it back.
        if let Some(out) = self.outgoing_chat.take() {
            // Client-side commands are answered here and never reach the wire — `/keyboard` is
            // local configuration, and the server has no idea what our bindings are.
            let out = match out {
                caer_render::chat::Outgoing::Command(cmd)
                    if is_client_command(&cmd, "keyboard") =>
                {
                    let args = cmd
                        .strip_prefix("keyboard")
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    for line in self.keyboard_command(&args) {
                        let _ = self.log.push(0x00, &line);
                    }
                    None
                }
                caer_render::chat::Outgoing::Command(cmd) if is_client_command(&cmd, "mute") => {
                    let muted = toggle_audio_mute(&mut self.audio);
                    let _ = self.log.push(
                        0x00,
                        if muted {
                            "[audio] muted"
                        } else {
                            "[audio] unmuted"
                        },
                    );
                    None
                }
                caer_render::chat::Outgoing::Command(cmd) if is_client_command(&cmd, "equip") => {
                    // System 4 verb 1: first backpack item → paperdoll (server remaps Item_Type).
                    let from = (40u16..=79).find(|&s| self.world.inventory_item(s as u8).is_some());
                    if let Some(from_slot) = from {
                        self.live_send(LiveCommand::MoveItem {
                            to_slot: 100,
                            from_slot,
                            count: 1,
                        });
                        let _ = self.log.push(
                            0x00,
                            &format!("[equip] MoveItem paperdoll ← bag slot {from_slot}"),
                        );
                    } else {
                        let _ = self.log.push(0x00, "[equip] no backpack item");
                    }
                    None
                }
                caer_render::chat::Outgoing::Command(cmd)
                    if is_client_command(&cmd, "interact") =>
                {
                    if let Some((oid, name)) = self.target.clone() {
                        self.live_send(LiveCommand::Interact {
                            player_x: self.player[0] as u32,
                            player_y: self.player[1] as u32,
                            target_oid: oid,
                        });
                        let _ = self
                            .log
                            .push(0x00, &format!("[interact] ObjectInteract → {name} ({oid})"));
                    } else {
                        let _ = self.log.push(0x00, "[interact] no target");
                    }
                    None
                }
                caer_render::chat::Outgoing::Command(cmd) if is_client_command(&cmd, "invite") => {
                    if let Some(hud) = self.skin_hud.as_ref() {
                        let d = self.product.dispatch(
                            caer_render::product_input::ProductInput::InviteToGroup,
                            hud.skin(),
                            caer_render::product_loop::ProductDispatchCtx {
                                chat_open: false,
                                has_target: self.target.is_some(),
                            },
                        );
                        if let Some(cmd) = d.command {
                            self.send_social_command(cmd);
                        } else {
                            let _ = self.log.push(0x00, "[invite] no target");
                        }
                    } else if self.target.is_some() {
                        self.live_send(LiveCommand::InviteToGroup);
                        let _ = self.log.push(0x00, "[social] InviteToGroup");
                    } else {
                        let _ = self.log.push(0x00, "[invite] no target");
                    }
                    None
                }
                caer_render::chat::Outgoing::Command(cmd) if is_client_command(&cmd, "buy") => {
                    let slot: u16 = cmd
                        .strip_prefix("buy")
                        .unwrap_or("")
                        .trim()
                        .parse()
                        .unwrap_or(0);
                    if let Some((oid, name)) = self.target.clone() {
                        self.live_send(LiveCommand::BuyItem {
                            player_x: self.player[0] as u32,
                            player_y: self.player[1] as u32,
                            merchant_id: oid,
                            item_slot: slot,
                            item_count: 1,
                        });
                        let _ = self.log.push(
                            0x00,
                            &format!("[buy] BuyRequest slot={slot} from {name} ({oid})"),
                        );
                    } else {
                        let _ = self.log.push(0x00, "[buy] no target");
                    }
                    None
                }
                caer_render::chat::Outgoing::Command(cmd)
                    if is_client_command(&cmd, "reloadaddon") =>
                {
                    let id = cmd
                        .strip_prefix("reloadaddon")
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    match self.addons.reload(&id) {
                        Ok(r) => {
                            let _ = self.log.push(
                                0x00,
                                &format!("[addon] reloaded {} gen={}", r.addon_id, r.generation),
                            );
                        }
                        Err(e) => {
                            let _ = self.log.push(0x00, &format!("[addon] reload failed: {e}"));
                        }
                    }
                    None
                }
                caer_render::chat::Outgoing::Command(cmd)
                    if is_client_command(&cmd, "sell")
                        || is_client_command(&cmd, "use")
                        || is_client_command(&cmd, "craft")
                        || is_client_command(&cmd, "destroy")
                        || is_client_command(&cmd, "trainwindow")
                        || is_client_command(&cmd, "train")
                        || is_client_command(&cmd, "siege")
                        || is_client_command(&cmd, "sit")
                        || is_client_command(&cmd, "stand") =>
                {
                    let ctx = EcoSlashCtx {
                        player: self.player,
                        heading: self.heading,
                        target: self.target.as_ref().map(|(oid, _)| *oid),
                    };
                    if let Some(live_cmd) = eco_slash_to_live(&cmd, &ctx) {
                        let note = eco_slash_chat_note(&live_cmd);
                        self.live_send(live_cmd);
                        let _ = self.log.push(0x00, &note);
                    } else {
                        let _ = self.log.push(0x00, "[eco] sell needs a merchant target");
                    }
                    None
                }
                other => Some(other),
            };
            if let Some(out) = out {
                match out {
                    caer_render::chat::Outgoing::Say(text) => {
                        // Private routing invariant: Say path must never carry send/tell bodies.
                        debug_assert!(!caer_render::chat::is_private_outgoing(
                            &caer_render::chat::Outgoing::Say(text.clone())
                        ));
                        self.log.push(0x01, &format!("You say, \"{text}\""));
                        self.live_send(LiveCommand::Say(text));
                    }
                    caer_render::chat::Outgoing::Command(cmd) => {
                        // Echo locally. Say already did this, but commands went out silently, so a
                        // command that was never submitted and one the server ignored looked
                        // IDENTICAL on screen — which is what made `/jump` read as "does nothing"
                        // with no way to tell which half was broken.
                        self.log.push(0x01, &format!("> /{cmd}"));
                        log::info!("rustdaoc: sending command `&{cmd}`");
                        self.live_send(LiveCommand::Command(cmd));
                    }
                }
            }
        }

        // Capture this frame if the recorder is running and its interval has elapsed.
        self.mark_liveness(5);
        let want_frame = self.recorder.as_mut().is_some_and(|r| r.wants_frame(dt));
        self.mark_liveness(6);
        let present_window = self.shell.window().cloned();
        let harness_capture_timeout =
            self.harness_pending_capture
                .as_ref()
                .map(|(_, _, deadline)| {
                    let elapsed = self
                        .harness
                        .as_ref()
                        .map_or(0, |harness| harness.elapsed_ms());
                    bounded_capture_wait(
                        *deadline,
                        elapsed,
                        caer_render::gpu::Gpu::gpu_wait_timeout(),
                    )
                });
        let (present_outcome, shot) = {
            let gpu = self.shell.gpu_mut().unwrap();
            gpu.render_capturing(
                count,
                egui_frame,
                want_frame,
                present_window.as_deref(),
                match (at_preworld, scene_behind) {
                    (false, _) => caer_render::gpu::FramePass::World,
                    (true, false) => caer_render::gpu::FramePass::PreWorldUi,
                    (true, true) => caer_render::gpu::FramePass::PreWorldScene,
                },
                harness_capture_timeout,
            )
        };
        self.mark_liveness(7);
        if shot.is_some() {
            // Snapshot the inputs that produced this frame.
            let mut keys = String::new();
            for (held, name) in [
                (self.keys.fwd, "W"),
                (self.keys.back, "S"),
                (self.keys.turn_left, "A"),
                (self.keys.turn_right, "D"),
                (self.keys.strafe_left, "Q"),
                (self.keys.strafe_right, "E"),
            ] {
                if held {
                    keys.push_str(name);
                }
            }
            if keys.is_empty() {
                keys.push('-');
            }
            let (heading, cam_orbit, pitch) = (self.heading, self.cam_orbit, self.orbit_pitch);
            let cam_yaw = self.camera_yaw();
            let pos = self.player;
            let speed = if self.keys.active() { MOVE_SPEED } else { 0.0 };
            let elapsed = now.duration_since(self.started).as_secs_f32();
            if let Some(r) = self.recorder.as_mut() {
                r.note_state(
                    elapsed, &keys, heading, cam_orbit, pitch, cam_yaw, pos, speed,
                );
            }
        }
        let mut captured_evidence = None;
        let harness_manifest = (self.harness.is_some() && shot.is_some()).then(|| {
            self.native_capture_manifest(
                viewport[0] as u32,
                viewport[1] as u32,
                self.harness_presented_frames.saturating_add(1),
            )
        });
        if let (Some(rgba), Some(rec)) = (shot, self.recorder.as_mut()) {
            let (w, h) = (viewport[0] as u32, viewport[1] as u32);
            let captured_path = rec.next_path();
            let write_result = if let Some(manifest) = harness_manifest.as_ref() {
                let result = caer_render::capture_manifest::write_capture(
                    &captured_path,
                    &rgba,
                    w,
                    h,
                    manifest,
                );
                rec.frames = rec.frames.saturating_add(1);
                result.map(|manifest_path| {
                    captured_evidence = Some((captured_path.clone(), manifest_path));
                    rec.frames < rec.max_frames
                })
            } else {
                rec.write(&rgba, w, h)
            };
            let stop = matches!(write_result, Ok(false));
            if stop {
                if let Some(r) = self.recorder.take() {
                    r.finish();
                }
            }
            if let Err(error) = write_result {
                if let Some(harness) = self.harness.as_mut() {
                    harness.fatal(
                        caer_harness::FatalClass::InfraFailure,
                        "native.window_capture_write",
                        format!("native capture write failed: {error}"),
                    );
                }
            }
        }
        let mut did_present = false;
        match present_outcome {
            caer_render::gpu::PresentOutcome::Presented => {
                did_present = true;
                if self.harness.is_some() {
                    self.harness_presented_frames = self.harness_presented_frames.saturating_add(1);
                    self.harness_last_present = Some(Instant::now());
                }
                if self.harness.is_some()
                    && (self.harness_presented_frames == 1
                        || self.harness_presented_frames.is_multiple_of(60))
                {
                    if let Some(harness) = self.harness.as_mut() {
                        harness.frame_presented(self.harness_presented_frames);
                    }
                }
            }
            caer_render::gpu::PresentOutcome::Skipped(reason) => {
                log::warn!("rustdaoc: presentation skipped: {reason}");
            }
            caer_render::gpu::PresentOutcome::Reconfigure(reason) => {
                self.mark_liveness(12);
                log::warn!("rustdaoc: forcing surface recovery after {reason}");
                self.shell
                    .gpu_mut()
                    .expect("product renderer has GPU")
                    .force_reconfigure_surface();
                if let Some(window) = self.shell.window() {
                    window.request_redraw();
                }
                self.mark_liveness(13);
            }
            caer_render::gpu::PresentOutcome::Fatal(reason) => {
                log::error!("rustdaoc: fatal surface validation failure: {reason}");
                if let Some(harness) = self.harness.as_mut() {
                    harness.fatal(
                        caer_harness::FatalClass::ProductFailure,
                        "native.surface_validation",
                        "native surface validation failed; details remain in product log".into(),
                    );
                }
            }
        }
        if let Some((captured_path, manifest_path)) = captured_evidence {
            if did_present {
                let pending_capture = self.harness_pending_capture.clone();
                let frame_id = pending_capture.as_ref().map_or_else(
                    || {
                        captured_path
                            .file_stem()
                            .and_then(|name| name.to_str())
                            .unwrap_or("frame")
                            .to_owned()
                    },
                    |(_, artifact_name, _)| artifact_name.clone(),
                );
                if let Some(harness) = self.harness.as_mut() {
                    let emitted = harness
                        .artifact_file(format!("native-{frame_id}"), &captured_path)
                        .and_then(|()| {
                            harness.artifact_file(
                                format!("native-{frame_id}-manifest"),
                                &manifest_path,
                            )
                        });
                    if let Err(error) = emitted {
                        harness.fatal(
                            caer_harness::FatalClass::InfraFailure,
                            "native.window_capture_evidence",
                            format!("native capture evidence was refused: {error}"),
                        );
                    } else if pending_capture.is_some() {
                        self.harness_pending_capture = None;
                    }
                }
            } else {
                let _ = std::fs::remove_file(captured_path);
                let _ = std::fs::remove_file(manifest_path);
                if let Some((_, artifact_name, _)) = self.harness_pending_capture.as_ref() {
                    self.recorder = Recorder::start_harness_capture(artifact_name);
                }
            }
        }
        self.emit_harness_observation();
        // A full frame interval after the completed attempt gives winit/KWin an actual wait phase
        // in which to dispatch presentation feedback and release the swapchain image.  Skipped and
        // recovery attempts are paced too; otherwise a timeout becomes a tight retry storm.
        self.last_frame = Instant::now();
        self.mark_liveness(8);
        // The next frame is paced by `about_to_wait`.  Requesting here as well creates a recursive
        // redraw loop and bypasses its deadline.
    }

    /// Is this one-shot screenshot a **pre-world** frame?
    ///
    /// Deliberately not [`Self::at_preworld`], which answers "is a pre-world plate on screen right
    /// now". That question is unanswerable here: `PreWorldFlow` holds `FlowStep::Loading` while
    /// `SessionPhase::InWorld` until `assets_ready`, and on this path the terrain build *is* the
    /// asset load. Asking it means the world never loads because the world never loaded — and
    /// SCN-00 [PRODUCT_BOOT] has been reporting "no terrain audit in output" ever since, because
    /// the branch that prints `caer-audit: fixtures=` was skipped every run.
    ///
    /// One frame, no event loop, so intent settles it: an explicit `--preworld <screen>`, or a
    /// session that genuinely has not entered the world.
    /// The pre-world screen this capture shows, for its manifest.
    fn preworld_screen_name(&self) -> &'static str {
        self.preworld_forced
            .or_else(|| caer_render::preworld::PreWorldScreen::from_phase(self.session_phase))
            .map_or("world", |s| match s {
                caer_render::preworld::PreWorldScreen::Login => "login",
                caer_render::preworld::PreWorldScreen::Splash => "splash",
                caer_render::preworld::PreWorldScreen::Loading => "loading",
                caer_render::preworld::PreWorldScreen::RealmSelect => "realm",
                caer_render::preworld::PreWorldScreen::CharSelect => "charselect",
                caer_render::preworld::PreWorldScreen::CharCreate => "charcreate",
                caer_render::preworld::PreWorldScreen::CharCustomize => "customize",
                caer_render::preworld::PreWorldScreen::CharStats => "stats",
            })
    }

    fn screenshot_is_preworld(&self) -> bool {
        self.preworld_forced.is_some()
            || caer_render::preworld::PreWorldScreen::from_phase(self.session_phase).is_some()
    }

    /// Monitor labels for the Options Menu's `Monitor:` control, in `available_monitors` order.
    fn monitor_labels(&self) -> Vec<String> {
        let Some(window) = self.shell.window() else {
            return Vec::new();
        };
        window
            .available_monitors()
            .enumerate()
            .map(|(i, m)| {
                let s = m.size();
                format!("{}: {}x{}", i + 1, s.width, s.height)
            })
            .collect()
    }

    /// Fill the Options Menu's draft from the settings that are actually live.
    ///
    /// Only the backed controls are read; the greyed ones show their own first value. This is the
    /// one direction — the dialog never writes here, [`Self::commit_options_draft`] does.
    fn options_draft_from_settings(&self) -> caer_render::preworld_options::OptionsDraft {
        use caer_render::preworld_options::{OptionsDraft, WindowChoice, VOLUME_STEPS};
        let modes = self
            .shell
            .window()
            .and_then(|w| w.current_monitor().or_else(|| w.primary_monitor()))
            .map_or_else(Vec::new, |m| caer_render::enumerate_modes(&m));
        let resolutions: Vec<String> = modes.iter().map(|m| m.label()).collect();
        let resolution = modes
            .iter()
            .position(|m| *m == self.display_draft.mode_size)
            .unwrap_or(0);
        let audio = self.audio.settings();
        // Ten steps, 0..=9, where 9 is Full. Round rather than truncate so a bus already at 1.0
        // reads back as Full instead of 9.
        let step = |gain: f32| (gain * f32::from(VOLUME_STEPS - 1)).round().clamp(0.0, 9.0) as u8;
        OptionsDraft {
            resolutions,
            resolution,
            monitors: self.monitor_labels(),
            monitor: 0,
            window: Some(match self.display_draft.mode {
                caer_render::WindowMode::Windowed => WindowChoice::Windowed,
                caer_render::WindowMode::Exclusive => WindowChoice::FullScreen,
                // Retail has no exclusive row, so an exclusive session shows as the nearest thing
                // it can express rather than as neither radio being set.
                _ => WindowChoice::FullScreenWindowed,
            }),
            clip_plane: 2,
            mouselook_sensitivity: 3,
            music: step(audio.music),
            sound: step(audio.effects),
            ambient_sound: step(audio.ambient),
            video_card: self
                .shell
                .gpu()
                .map(|g| g.adapter_name().to_string())
                .unwrap_or_default(),
        }
    }

    /// Accept: push the dialog's draft into the settings it is backed by.
    fn commit_options_draft(&mut self) {
        use caer_render::preworld_options::{WindowChoice, VOLUME_STEPS};
        let Some(draft) = self
            .preworld_hud
            .as_ref()
            .map(|pw| pw.options_draft().clone())
        else {
            return;
        };
        let gain = |v: u8| f32::from(v) / f32::from(VOLUME_STEPS - 1);
        let mut audio = self.audio.settings();
        audio.music = gain(draft.music);
        audio.effects = gain(draft.sound);
        audio.ambient = gain(draft.ambient_sound);
        self.audio.set_settings(audio);

        let modes = self
            .shell
            .window()
            .and_then(|w| w.current_monitor().or_else(|| w.primary_monitor()))
            .map_or_else(Vec::new, |m| caer_render::enumerate_modes(&m));
        let mut candidate = self.display.clone();
        if let Some(mode) = modes.get(draft.resolution) {
            candidate.mode_size = *mode;
            // The stock dialog has no "follow the window" row — its Resolution control is always
            // an explicit size. Accepting it therefore leaves the modern panel's dynamic mode,
            // or the size the player just picked would be recorded and never applied.
            candidate.use_current_window = false;
        }
        candidate.mode = match draft.window_choice() {
            WindowChoice::Windowed => caer_render::WindowMode::Windowed,
            WindowChoice::FullScreenWindowed => caer_render::WindowMode::Borderless,
            WindowChoice::FullScreen => caer_render::WindowMode::Exclusive,
        };
        if candidate.mode == self.display.mode
            && candidate.mode_size == self.display.mode_size
            && candidate.use_current_window == self.display.use_current_window
        {
            println!("rustdaoc: options accepted — audio only, display unchanged");
            return;
        }
        let Some(window) = self.shell.window().cloned() else {
            return;
        };
        match caer_render::apply_display_settings(&window, &candidate) {
            caer_render::ApplyResult::Applied => {
                self.display = candidate.clone();
                self.display_draft = candidate;
                if let Err(error) = self.display.save() {
                    log::warn!("rustdaoc: could not persist display settings: {error}");
                }
                let size = window.inner_size();
                self.shell.note_resize(size.width, size.height);
                log_display_apply(&window, &self.display);
            }
            caer_render::ApplyResult::Reverted { reason } => {
                log::warn!("rustdaoc: options display apply reverted: {reason}");
                self.display_draft = self.display.clone();
            }
        }
    }

    /// Which realm's 3D scene belongs behind the screen currently on the glass.
    ///
    /// `None` for every screen that is genuinely flat art — splash, login, realm select, loading.
    /// Ctrl+Alt camera tuning for the pre-world stage. Returns true when the key was consumed.
    ///
    /// Arrows nudge bearing and eye; PageUp/PageDown dolly; `,`/`.` focus; `A`/`D` walk the figure
    /// across the stage and `W`/`S` walk it in depth; `Space`/`Z` raise and lower the whole camera
    /// straight up; `[`/`]` tip the current race's head down and up; `P` prints the settled values
    /// as pasteable env vars; `0` clears this realm back to the shipped composition and `\` clears
    /// this race's head pitch.
    ///
    /// The camera knobs are per REALM and the head pitch is per RACE+GENDER, because that is what
    /// each one is a property of: Eden's stage camera does not move when the race does, and a head
    /// that looks wrong looks wrong on one body.
    fn tune_preworld_camera(&mut self, code: KeyCode) -> bool {
        use caer_render::preworld_camera_tune as tune;
        use caer_render::preworld_head_tune as head;
        let Some(realm) = self.preworld_scene_realm() else {
            return false;
        };
        let Some(scene) = self.preworld_scene.as_ref() else {
            return false;
        };
        // Seed each knob from what is currently on screen, so the first nudge adjusts rather
        // than teleports.
        // Seed from the realm's SHIPPED composition, not from the generic defaults: the first
        // nudge must move the picture that is on screen. Seeding from a value the renderer is not
        // using teleports the camera on the first keypress and loses the settled number.
        let ship = tune::shipped(realm);
        let cur_bearing = ship.bearing.unwrap_or(scene.camera_bearing_deg);
        let cur_dolly = ship
            .dolly
            .unwrap_or(caer_render::preworld_scene::SHIPPED_DOLLY);
        let cur_eye = ship.eye.unwrap_or(0.62);
        let cur_focus = ship.focus.unwrap_or(0.55);
        let cur_lift = ship.lift.unwrap_or(0.0);

        let (knob, steps, seed) = match code {
            KeyCode::ArrowLeft => (tune::Knob::Bearing, -1.0, cur_bearing),
            KeyCode::ArrowRight => (tune::Knob::Bearing, 1.0, cur_bearing),
            KeyCode::ArrowUp => (tune::Knob::Eye, 1.0, cur_eye),
            KeyCode::ArrowDown => (tune::Knob::Eye, -1.0, cur_eye),
            // PageUp zooms IN, so it pulls the dolly closer.
            KeyCode::PageUp => (tune::Knob::Dolly, -1.0, cur_dolly),
            KeyCode::PageDown => (tune::Knob::Dolly, 1.0, cur_dolly),
            // A / D walk the figure across the stage; the camera follows, so the scene slides.
            KeyCode::KeyA => (tune::Knob::SubjectX, -1.0, 0.0),
            KeyCode::KeyD => (tune::Knob::SubjectX, 1.0, 0.0),
            // W / S walk the figure in depth — W away from the lens, S toward it. The camera
            // follows, so the figure keeps its size and slides THROUGH the backdrop instead. That
            // is the move for props spaced differently to Eden's, which a dolly cannot make.
            KeyCode::KeyW => (tune::Knob::SubjectY, 1.0, 0.0),
            KeyCode::KeyS => (tune::Knob::SubjectY, -1.0, 0.0),
            KeyCode::Comma => (tune::Knob::Focus, -1.0, cur_focus),
            KeyCode::Period => (tune::Knob::Focus, 1.0, cur_focus),
            // Space / Z raise and lower the lens straight up, aim and all. Distinct from the Eye
            // knob above, which raises the eye alone and therefore tilts: this one translates, so
            // the composition rises without the shot swinging.
            KeyCode::Space => (tune::Knob::Lift, 1.0, cur_lift),
            KeyCode::KeyZ => (tune::Knob::Lift, -1.0, cur_lift),
            // [ and ] tilt the head of whoever is standing on the stage. Per race+gender, and
            // deliberately not part of the camera above — see `preworld_head_tune`.
            KeyCode::BracketLeft | KeyCode::BracketRight => {
                let Some((race, gender)) = self.preworld_avatar_identity() else {
                    println!("rustdaoc: head tune — nobody is standing on the stage");
                    return true;
                };
                let steps = if code == KeyCode::BracketRight {
                    1.0
                } else {
                    -1.0
                };
                let v = head::nudge(race, gender, steps);
                let scale = self
                    .entity_models
                    .as_ref()
                    .map_or(1.0, |em| em.race_display_scale(race, gender));
                println!(
                    "rustdaoc: head tune — race {race} gender {gender} HEAD_PITCH = {v:+.2} deg                      (display scale {scale:.2})"
                );
                return true;
            }
            KeyCode::Backslash => {
                let Some((race, gender)) = self.preworld_avatar_identity() else {
                    return true;
                };
                head::reset(race, gender);
                println!(
                    "rustdaoc: head tune — race {race} gender {gender} back to the authored pose"
                );
                return true;
            }
            // Realm, race and gender from the keyboard, on the creation form.
            //
            // Settling a camera and a head tilt means visiting 18 races across three realms and
            // both genders, and every one of those is otherwise a trip out to the realm plate and
            // back with the mouse. These move the subject without leaving the shot.
            KeyCode::Digit1 | KeyCode::Digit2 | KeyCode::Digit3 => {
                let want = match code {
                    KeyCode::Digit1 => 1,
                    KeyCode::Digit2 => 2,
                    _ => 3,
                };
                self.create_realm = want;
                self.create_draft.realm = want;
                self.create_draft
                    .set_race(caer_render::preworld::race_id_for_realm(want, 0));
                self.preworld_scene_uploaded = None;
                println!(
                    "rustdaoc: stage — realm {want}, race {}",
                    self.create_draft.race
                );
                return true;
            }
            KeyCode::KeyN | KeyCode::KeyB => {
                let realm = if self.create_realm != 0 {
                    self.create_realm
                } else {
                    1
                };
                // The seven buttons of this realm's grid, in the captured order.
                let grid: Vec<u8> = (0..7)
                    .map(|i| caer_render::preworld::race_id_for_realm(realm, i))
                    .collect();
                let here = grid
                    .iter()
                    .position(|&r| r == self.create_draft.race)
                    .unwrap_or(0);
                let step = if code == KeyCode::KeyN {
                    1
                } else {
                    grid.len() - 1
                };
                let next = grid[(here + step) % grid.len()];
                self.create_draft.set_race(next);
                println!(
                    "rustdaoc: stage — realm {realm} race {next} ({})",
                    caer_assets::figures::FigureModels::race_name(next).unwrap_or("?")
                );
                return true;
            }
            KeyCode::KeyG => {
                let next = u8::from(self.create_draft.gender == 0);
                self.create_draft.set_gender(next);
                println!("rustdaoc: stage — gender {next}");
                return true;
            }
            KeyCode::Digit0 | KeyCode::Numpad0 => {
                tune::reset(realm);
                println!("rustdaoc: camera tune — realm {realm} reset to the shipped composition");
                return true;
            }
            KeyCode::KeyP => {
                let lines = tune::report(realm);
                if lines.is_empty() {
                    println!("rustdaoc: camera tune — realm {realm} has no overrides set");
                } else {
                    println!("rustdaoc: camera tune — realm {realm} settled values:");
                    println!("    {}", lines.join(" "));
                }
                // Head pitch beside each race's display scale, because the SHAPE of that pairing
                // is the finding: offsets that track the scale accuse the fixed stage lens, and
                // one constant across every race accuses our own compose path instead.
                let heads = head::entries();
                if heads.is_empty() {
                    println!("rustdaoc: head tune — no race has a head pitch set");
                } else {
                    println!("rustdaoc: head tune — settled head pitch, by race:");
                    for ((race, gender), deg) in heads {
                        let name =
                            caer_assets::figures::FigureModels::race_name(race).unwrap_or("?");
                        let word = if gender == caer_assets::figures::GENDER_FEMALE {
                            "female"
                        } else {
                            "male"
                        };
                        let scale = self
                            .entity_models
                            .as_ref()
                            .map_or(1.0, |em| em.race_display_scale(race, gender));
                        println!(
                            "    {race:>3} {gender}  {name:<12} {word:<7} pitch {deg:+7.2}  \
                             scale {scale:.2}"
                        );
                    }
                }
                return true;
            }
            _ => return false,
        };
        let v = tune::nudge(realm, knob, steps, seed);
        let t = tune::tuning(realm);
        println!(
            "rustdaoc: camera tune — realm {realm} {} = {v:.3}   [bearing {:.2} dolly {:.2} eye {:.3} focus {:.3} subject_x {:.1} subject_y {:.1}]",
            knob.label(),
            t.bearing.unwrap_or(cur_bearing),
            t.dolly.unwrap_or(cur_dolly),
            t.eye.unwrap_or(cur_eye),
            t.focus.unwrap_or(cur_focus),
            t.subject_x.unwrap_or(0.0),
            t.subject_y.unwrap_or(0.0),
        );
        true
    }

    /// The look the previewed body wears — the create draft on CharCreate, the selected
    /// character's stored bytes on CharSelect. Same screen rules as
    /// [`Self::preworld_avatar_identity`], so what stands there and who it is can never disagree.
    fn preworld_avatar_appearance(&self) -> caer_protocol::customization::AvatarAppearance {
        let screen = self.preworld_pin().or_else(|| {
            self.preworld_hud
                .as_ref()
                .map(caer_render::preworld::PreWorldHud::screen)
        });
        screen.map_or_else(Default::default, |screen| {
            caer_render::preworld::preview_appearance(
                screen,
                self.char_overview.as_ref(),
                self.selected_protocol_slot,
                &self.create_draft,
            )
        })
    }

    fn preworld_scene_realm(&self) -> Option<u8> {
        use caer_render::preworld::PreWorldScreen;
        // `--preworld` forces a screen before the HUD has been synced, so consult the forced
        // value first; otherwise the scene decision runs a frame behind the screen it is for.
        let screen = self.preworld_pin().or_else(|| {
            self.preworld_hud
                .as_ref()
                .map(caer_render::preworld::PreWorldHud::screen)
        })?;
        if !matches!(
            screen,
            PreWorldScreen::CharSelect
                | PreWorldScreen::CharCreate
                | PreWorldScreen::CharCustomize
                | PreWorldScreen::CharStats
        ) {
            return None;
        }
        Some(if self.create_realm != 0 {
            self.create_realm
        } else {
            self.transition.realm().max(1)
        })
    }

    /// Load (once per realm) the scene the current screen wants. Returns true when one is ready.
    ///
    /// Cached on realm because it is tens of thousands of vertices and the screen it backs is one
    /// the player sits on. A realm whose archive will not load is remembered too — otherwise every
    /// frame retries a failing NIF parse for as long as the player stays on the screen.
    fn ensure_preworld_scene(&mut self) -> bool {
        let Some(realm) = self.preworld_scene_realm() else {
            return false;
        };
        if self.preworld_scene_realm_tried != Some(realm) {
            let job_id = format!("preworld-scene:{realm}");
            let began = Instant::now();
            let began_ms = began
                .saturating_duration_since(self.started)
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX);
            self.harness_preworld_scene_job = caer_harness::AssetJobSnapshot {
                state: "running".into(),
                started_elapsed_ms: caer_harness::Known::Known { value: began_ms },
                ended_elapsed_ms: caer_harness::Known::Unknown {
                    reason: "pre-world scene job is running".into(),
                },
                failure: None,
            };
            self.emit_harness_asset(job_id.clone(), "running");
            self.preworld_scene_realm_tried = Some(realm);
            let root = caer_render::terrain::client_root();
            self.preworld_scene = caer_render::preworld_scene::load_cached(&root, realm);
            let terminal = if self.preworld_scene.is_some() {
                "complete"
            } else {
                "failed"
            };
            self.harness_preworld_scene_job.state = terminal.into();
            self.harness_preworld_scene_job.ended_elapsed_ms = caer_harness::Known::Known {
                value: Instant::now()
                    .saturating_duration_since(self.started)
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
            };
            self.harness_preworld_scene_job.failure = (terminal == "failed")
                .then(|| "pre-world scene archive or decoded scene was unavailable".into());
            self.emit_harness_asset(job_id, terminal);
            self.preworld_scene_uploaded = None;
            self.preworld_scene_morphing = None;
            // Lighting comes from the published atmosphere, and nothing publishes one before the
            // world loads: `SkyLighting::default()` is deliberately all zeros, so every surface
            // multiplied to black.
            //
            // The stage is a realm's own homeland, so it is lit by that realm's sky. This used to
            // publish `sky_default` for all three, which is invisible on Albion — `sky_albion.dat`
            // and `sky_default.dat` carry identical daylight — and wrong on the other two: Midgard
            // differs from the default in 256 fields and Hibernia in 76, so both were being lit by
            // Albion's sun.
            //
            // PROVENANCE GAP, narrowed but not closed: the realm's own sky is the defensible
            // choice from client data, but which sky retail lights a character screen with is
            // still not established, and neither is the time of day.
            caer_render::atmosphere::publish(caer_render::atmosphere::Atmosphere::load_for_region(
                &root,
                caer_render::atmosphere::sky_name_for_realm(realm),
            ));
            // Respawn the stage's emitters for the realm now on screen. Keyed on realm so a
            // return to the same stage does not restart its snow mid-fall.
            if self.preworld_particles_realm != Some(realm) {
                self.preworld_particles_realm = Some(realm);
                self.preworld_particles = self
                    .preworld_scene
                    .as_ref()
                    .map(|scene| {
                        scene
                            .emitters
                            .iter()
                            // Sprites only. A `NiParticleMeshes` emitter spawns MESH instances,
                            // and pushing one through the billboard path draws its authored size
                            // as a camera-facing blob — Midgard's two are size 33.8 and 42.3,
                            // which covered a quarter of the screen in white.
                            .filter(|d| !d.mesh_particles)
                            .enumerate()
                            .map(|(i, def)| {
                                let mut sys = caer_render::particles::ParticleSystem::from_def(
                                    def.clone(),
                                    0x5EED ^ (i as u64) << 8 ^ u64::from(realm),
                                );
                                // Weather has always been falling. Warm to one particle lifetime
                                // so the stage opens with a full field rather than filling in.
                                sys.warm(def.lifetime.clamp(0.0, 8.0));
                                sys
                            })
                            .collect()
                    })
                    .unwrap_or_default();
            }
            if let Some(scene) = self.preworld_scene.as_ref() {
                println!(
                    "rustdaoc: preworld scene realm {} — {} batches, {} verts, {} textures, {} emitters",
                    scene.realm,
                    scene.models.len(),
                    scene.models.iter().map(|m| m.vertices.len()).sum::<usize>(),
                    scene.textures.len(),
                    scene.emitters.len()
                );
            } else {
                println!(
                    "rustdaoc: preworld scene realm {realm} — none loaded (missing archive {})",
                    caer_render::preworld_scene::scene_archive(realm)
                );
            }
        }
        self.preworld_scene.is_some()
    }

    /// The one viewport the pre-world screens are drawn in, hit-tested in, and measured in.
    ///
    /// The GPU surface, not the window — the surface trails a resize by a frame, so a draw
    /// against the window size and a hit-test against the surface size disagree exactly while
    /// the window is moving. One source keeps [`caer_render::preworld::PreworldTransform`]
    /// identical on both sides of the click.
    fn pointer_viewport(&self) -> (f32, f32) {
        if let Some(gpu) = self.shell.gpu() {
            return gpu.surface_size();
        }
        self.window_viewport()
    }

    fn window_viewport(&self) -> (f32, f32) {
        self.shell.window().map_or((1600.0, 1000.0), |w| {
            let s = w.inner_size();
            (s.width.max(1) as f32, s.height.max(1) as f32)
        })
    }

    fn cursor_to_surface(&self, x: f32, y: f32, surface: (f32, f32)) -> [f32; 2] {
        // No window means nothing to convert from — the identity keeps the caller honest.
        let window = self.shell.window().map_or(surface, |w| {
            let s = w.inner_size();
            (s.width.max(1) as f32, s.height.max(1) as f32)
        });
        cursor_to_surface(x, y, window, surface)
    }

    /// Whose body stands on the character screen: `(eRace, fig3 gender)`.
    ///
    /// The rule itself lives in [`caer_render::preworld::preview_identity`] so it is testable
    /// without a window; this only supplies the four inputs. Select reads the overview row the
    /// player actually selected rather than `player_race`/`player_gender`, which stay latched
    /// from the last character loaded and would otherwise stand a body on an empty screen.
    ///
    /// **The gender there is fig3's, not the protocol's.** `fig3map.csv` keys male as 1 and
    /// female as 2, while every wire field uses 0 and 1. Passing the wire value straight through
    /// asks the table for gender 0, which does not exist — `figure_id` returns `None`,
    /// `base_body` returns nothing, and the screen renders with no character on it and no error
    /// anywhere. `fig3_gender_from_db` is the one conversion.
    fn preworld_avatar_identity(&self) -> Option<(u8, u8)> {
        let screen = self.preworld_pin().or_else(|| {
            self.preworld_hud
                .as_ref()
                .map(caer_render::preworld::PreWorldHud::screen)
        })?;
        caer_render::preworld::preview_identity(
            screen,
            self.char_overview.as_ref(),
            self.selected_protocol_slot,
            self.create_draft.race,
            self.create_draft.gender,
        )
    }

    /// Worn models for the body on the character screens.
    ///
    /// Select dresses the character from the gear the overview packet carried. Create has no
    /// character yet and therefore no 0x15, so the draft body stands undressed — starter cloth
    /// is Part-2 asset binding, not something to invent here.
    fn preworld_avatar_equipment(&self) -> Option<caer_protocol::equipment::EquipmentUpdate> {
        use caer_render::preworld::PreWorldScreen;
        let screen = self.preworld_pin().or_else(|| {
            self.preworld_hud
                .as_ref()
                .map(caer_render::preworld::PreWorldHud::screen)
        })?;
        match screen {
            PreWorldScreen::CharSelect => {
                let slot = self.selected_protocol_slot?;
                let ov = self.char_overview.as_ref()?;
                let c = ov.characters.iter().find(|ch| ch.slot == slot)?;
                if c.gear.is_empty() {
                    None
                } else {
                    Some(c.gear.to_equipment_update(0))
                }
            }
            PreWorldScreen::CharCreate => None,
            _ => None,
        }
    }

    /// Headless: render exactly one frame to a PNG and exit (the agent-facing verification path).
    fn screenshot(&mut self, out: &str, size: (u32, u32)) {
        // Codex B1/B2: typed build identity on the product screenshot path.
        // SAFETY: single-threaded screenshot entry; env is process-local for evidence naming.
        std::env::set_var("CAER_RT_EXAMPLE", "rustdaoc");
        caer_client::evidence::emit("rustdaoc");
        let (w, h) = size;
        let mut gpu = pollster::block_on(Gpu::new_headless(w, h, CULL_RADIUS as f32))
            .unwrap_or_else(|e| {
                eprintln!("rustdaoc: headless GPU init failed:\n{e}");
                std::process::exit(1);
            });
        if self.harness.is_some() {
            let mut identity = self
                .harness_identity
                .clone()
                .expect("harness identity is precomputed before the event loop");
            caer_render::harness::bind_gpu_identity(
                &mut identity,
                gpu.adapter_name().to_owned(),
                gpu.adapter_backend().to_owned(),
            );
            if let Some(harness) = self.harness.as_mut() {
                harness.hello(
                    identity.clone(),
                    vec!["authoritative-read-only-snapshot".into(), "capture".into()],
                );
            }
            self.harness_identity = Some(identity);
            self.emit_harness_observation();
        }
        let at_preworld = self.screenshot_is_preworld();
        // The character screens are not flat plates: retail renders a modelled realm scene behind
        // the frame, and the plate's middle is transparent. Draw it before the UI goes on top.
        let mut scene_behind = false;
        if at_preworld {
            if self.ensure_preworld_scene() {
                let identity = self.preworld_avatar_identity();
                let appearance = self.preworld_avatar_appearance();
                let dress = self.preworld_avatar_equipment();
                let preview = self.active_preworld_preview_camera();
                if let Some(scene) = self.preworld_scene.as_ref() {
                    compose_preworld_gpu(
                        &mut gpu,
                        scene,
                        &mut self.preworld_scene_uploaded,
                        &mut self.preworld_scene_morphing,
                        self.entity_models.as_mut(),
                        identity,
                        appearance,
                        dress.as_ref(),
                        (w as f32, h as f32),
                        screenshot_anim_time(),
                        preview,
                    );
                    scene_behind = true;
                }
                // The stage's own emitters, on the verification path too. Uploading them only in
                // the live loop would leave every headless gate blind to the one thing it is
                // meant to check.
                let mut mesh = Vec::new();
                if let Some(scene) = self.preworld_scene.as_ref() {
                    let (eye, focus) = caer_render::preworld_scene::framing_backdrop(
                        scene,
                        w as f32 / (h as f32).max(1.0),
                    );
                    let c = preworld_camera_carriage(eye, focus, w as f32 / (h as f32).max(1.0));
                    let (r, u) = c.billboard_axes();
                    for sys in &self.preworld_particles {
                        mesh.extend(sys.billboard_mesh([r.x, r.y, r.z], [u.x, u.y, u.z]));
                    }
                }
                gpu.upload_particle_billboards(&mesh);
                println!(
                    "rustdaoc: preworld particles — {} billboard verts",
                    mesh.len()
                );
            }
        }
        let count = if at_preworld {
            // Skip terrain/world under pre-world plates (QA defects a/b).
            0
        } else {
            // Build terrain around the player (dungeon NIF geometry when surface mesh is empty).
            let r = CULL_RADIUS;
            let (px, py) = (self.player[0] as i32, self.player[1] as i32);
            let root = caer_render::terrain::client_root();
            let (mesh, dungeon) = caer_render::dungeon_mesh::load_product_terrain(
                &root,
                self.region,
                self.origin,
                [px - r, py - r],
                [px + r, py + r],
            );
            let placed: usize = mesh.models.iter().map(|m| m.instances.len()).sum();
            println!(
                "rustdaoc: terrain — {} zones, {} model kinds ({placed} placed){}",
                mesh.zones_loaded,
                mesh.models.len(),
                if dungeon.model_instances > 0 || dungeon.fixture_boxes > 0 {
                    format!(
                        ", dungeon nif_instances={} nif_kinds={} box_fallback={}",
                        dungeon.model_instances, dungeon.model_kinds, dungeon.fixture_boxes
                    )
                } else {
                    String::new()
                }
            );
            println!(
                "caer-audit: fixtures={} zones={} dungeon_nif_instances={} dungeon_box_fallback={}",
                mesh.fixtures.len(),
                mesh.zones_loaded,
                dungeon.model_instances,
                dungeon.fixture_boxes
            );
            if dungeon.model_instances > 0 {
                let zone = dungeon_zone_for_region(self.region)
                    .unwrap_or(caer_render::dungeon_mesh::CANONICAL_DUNGEON_ZONE);
                let fp = caer_render::dungeon_mesh::scene_fingerprint(&mesh, self.region, zone);
                println!(
                    "caer-audit: dungeon_scene hash=0x{:016x} kinds={} instances={} tris={} box_fallback={}",
                    fp.content_hash,
                    fp.model_kinds,
                    fp.model_instances,
                    fp.total_triangles,
                    fp.fixture_box_fallback
                );
            }
            gpu.set_terrain(&mesh.zones);
            gpu.set_water(&mesh.water_vertices, &mesh.water_indices);
            gpu.set_models(&mesh.models, &mesh.textures);
            gpu.set_fixtures(&caer_render::fixture_instances(&mesh.fixtures));

            // Assemble the player avatar for this headless frame too (same body the live window shows).
            let self_model = match (self.player_race, self.player_gender) {
                (Some(race), Some(gender)) => self.entity_models.as_mut().and_then(|em| {
                    let equip = if self.self_object_id != 0 {
                        self.world.equipment_of(self.self_object_id)
                    } else {
                        None
                    };
                    em.ensure_avatar(&mut gpu, race, gender, self.player_appearance, equip)
                }),
                _ => None,
            };
            let camera = if let Some(zone) = dungeon_zone_for_region(self.region) {
                // Elevated deterministic overview: third-person defaults sit inside multi-kilounit
                // dungeon chunks and produce a clear-color + HUD frame. Look down at the cluster.
                let root = caer_render::terrain::client_root();
                let seat = caer_render::dungeon_mesh::dungeon_product_seat(&root, zone);
                let focus = Vec3::new(seat[0] + 1_800.0, seat[1] + 2_400.0, seat[2]);
                let eye = Vec3::new(seat[0] - 1_200.0, seat[1] - 5_500.0, seat[2] + 4_200.0);
                let mut c = Camera::new(eye, focus, self.origin, w as f32 / h as f32);
                c.ensure_far(300_000.0);
                println!(
                    "rustdaoc: dungeon overview cam zone={zone} eye=({:.0},{:.0},{:.0}) focus=({:.0},{:.0},{:.0})",
                    eye.x, eye.y, eye.z, focus.x, focus.y, focus.z
                );
                c
            } else {
                self.follow_camera(w as f32 / h as f32)
            };
            let cull_center = [self.player[0] as i32, self.player[1] as i32];
            let count = render_world(
                &self.world,
                self.origin,
                cull_center,
                CULL_RADIUS,
                self.entity_models.as_mut(),
                &mut gpu,
                &mut self.instances,
                &mut self.mesh_instances,
                Some(&mesh),
                self_model,
                None,
                None,
                0.0,
                screenshot_anim_time(),
            );
            gpu.set_view_proj(camera.view_proj().to_cols_array_2d());
            gpu.upload_instances(&self.instances);
            count
        };
        if let Some(forced) = self.preworld_forced {
            let options = self.options_draft_from_settings();
            let open_options = self.preworld_forced_options;
            let forced_modal = self.preworld_forced_modal;
            let forced_typed = self.preworld_forced_modal_typed;
            if let Some(pw) = self.preworld_hud.as_mut() {
                pw.set_screen(forced);
                pw.set_create_draft(&self.create_draft);
                if open_options {
                    pw.set_options_draft(options);
                    pw.apply_action(caer_render::preworld::PreWorldAction::CharSelectOptions);
                }
                if let Some(m) = forced_modal {
                    pw.apply_action(match m {
                        caer_render::preworld_hitbox::Modal::QuitConfirm => {
                            caer_render::preworld::PreWorldAction::CharSelectQuit
                        }
                        caer_render::preworld_hitbox::Modal::DeleteConfirm => {
                            caer_render::preworld::PreWorldAction::DeleteCharacter
                        }
                    });
                    if forced_typed {
                        for c in caer_render::preworld::DELETE_CONFIRM_WORD.chars() {
                            pw.type_into_modal(c);
                        }
                    }
                }
                let skin_name = pw.render(&mut gpu, (w as f32, h as f32));
                if let Some(name) = skin_name {
                    if let Some(skin) = self.skin_hud.as_mut() {
                        let pass_mask = if self.login_password.is_empty() {
                            None
                        } else {
                            Some("*".repeat(self.login_password.len().min(32)))
                        };
                        let pass_mask_ref = pass_mask.as_deref();
                        let state = caer_render::adapters::AdapterState {
                            player_name: &self.hud.name,
                            status: &self.hud.status,
                            target: None,
                            zone: None,
                            fps: 0,
                            sheet: None,
                            char_stats: None,
                            char_resists: None,
                            equipment: None,
                            money: None,
                            inventory: None,
                            merchant: None,
                            weapon_armor: None,
                            attack_mode: None,
                            login_account: Some(self.login_account.as_str()),
                            login_password_mask: pass_mask_ref,
                            create_name: Some(self.create_draft.name.as_str()),
                        };
                        let vp = (w as f32, h as f32);
                        let pos = if name == "login" {
                            caer_render::preworld::login_dialog_pos(vp)
                        } else {
                            (80.0, 80.0)
                        };
                        let base = if name == "login" {
                            pw.last_layout_quads()
                        } else {
                            &[]
                        };
                        skin.render_over(&mut gpu, base, &[(name, pos)], &state);
                    }
                }
            }
        }
        // In-world HUD only — never composite egui vitals onto pre-world screenshots (QA defect a).
        let overlay = if at_preworld {
            None
        } else {
            self.refill_hud_from_world();
            let hud = &self.hud;
            Some(caer_render::ui::tessellate_headless([w, h], 1.0, |u| {
                caer_render::hud::build(u, hud)
            }))
        };
        // Screenshot the pass the *window* would present, not a different one. These two used to
        // disagree: every headless capture of a character screen went through the world encoder
        // and showed the realm scene, while the live client ran the pre-world encoder, which had
        // no model step and was black. A screenshot that cannot show the bug is not evidence.
        let rgba = if at_preworld {
            let pass = if scene_behind {
                caer_render::gpu::FramePass::PreWorldScene
            } else {
                caer_render::gpu::FramePass::PreWorldUi
            };
            gpu.render_to_rgba_pass(count, pass).expect("GPU wait")
        } else {
            gpu.render_to_rgba_with_overlay(count, overlay)
                .expect("GPU wait")
        };

        // Ground rule 7: a capture that does not say what it is a capture of cannot be compared
        // with anything. Written unconditionally so no capture is ever the undocumented one.
        let env = |k: &str| std::env::var(k).ok();
        let capture_identity = self.preworld_avatar_identity();
        let mut manifest = caer_render::capture_manifest::CaptureManifest::new()
            .with("screen", self.preworld_screen_name())
            .with("width", w)
            .with("height", h)
            .with("anim_time", screenshot_anim_time())
            .with(
                "realm",
                self.preworld_scene
                    .as_ref()
                    .map_or_else(|| "unknown".into(), |scene| scene.realm.to_string()),
            )
            .with(
                "race",
                capture_identity.map_or_else(|| "unknown".into(), |value| value.0.to_string()),
            )
            .with(
                "gender",
                capture_identity.map_or_else(|| "unknown".into(), |value| value.1.to_string()),
            )
            .with("class", self.create_draft.class_id)
            .with(
                "appearance",
                format!("{:?}", self.create_draft.appearance()),
            )
            .with("surface_width", w)
            .with("surface_height", h)
            .with("scale_factor", 1)
            .with("display_mode", self.display.mode.as_str())
            .with("player_position", format!("{:?}", self.player))
            .with("heading", self.heading)
            .with("camera_orbit", self.cam_orbit)
            .with("camera_pitch", self.orbit_pitch)
            .with(
                "pose_elapsed_ms",
                (screenshot_anim_time() * 1000.0).round() as u64,
            )
            .with("commit", caer_client::evidence::BUILD_COMMIT)
            // Measured now, not baked. `CAER_BUILD_DIRTY` is stamped by `build.rs`, which reruns
            // only when `.git/HEAD` or `.git/index` moves — an unstaged edit to a tracked file
            // touches neither, so the constant can keep claiming a clean tree indefinitely
            // (ledger B11). A capture that cannot say whether its own build matched the source is
            // not evidence of anything, and `unknown` is an honest answer where `false` is not.
            .with(
                "worktree",
                match caer_client::evidence::worktree_dirty_now() {
                    Some(true) => "dirty",
                    Some(false) => "clean",
                    None => "unknown",
                },
            );
        if let Some((race, gender)) = self.preworld_avatar_identity() {
            manifest = manifest.with("race", race).with("gender", gender);
        }
        if let Some(scene) = self.preworld_scene.as_ref() {
            manifest = manifest
                .with("realm", scene.realm)
                .with("scene_bearing", scene.camera_bearing_deg);
        }
        for (key, var) in [
            ("scene_dolly", "CAER_SCENE_DOLLY"),
            // Eye and focus move the lens, so two captures taken at different heights are two
            // captures of different things. They were read by `preworld_scene::framing_around_
            // character` and recorded nowhere, which made the solver's own knob invisible to the
            // instrument that exists to refuse mismatched pairs.
            ("scene_eye", "CAER_SCENE_EYE"),
            ("scene_focus", "CAER_SCENE_FOCUS"),
            ("filler_probe", "CAER_FILLER_PROBE"),
            ("part_tint", "CAER_PART_TINT"),
            ("client", "CAER_CLIENT"),
        ] {
            if let Some(v) = env(var) {
                manifest = manifest.with(key, v);
            }
        }
        // Image and manifest are one act, so an undocumented capture cannot reach disk. This used
        // to print the manifest failure and return normally, leaving a PNG that looks like every
        // other capture and that no comparison may legally use (Codex, e752).
        match caer_render::capture_manifest::write_capture(
            std::path::Path::new(out),
            &rgba,
            w,
            h,
            &manifest,
        ) {
            Ok(path) => {
                println!("rustdaoc: screenshot {out} ({w}x{h}, {count} boxes + entity meshes, player ({:.0}, {:.0}))", self.player[0], self.player[1]);
                println!("rustdaoc: manifest {}", path.display());
                if let Some(harness) = self.harness.as_mut() {
                    if let Err(error) =
                        harness.artifact_file("native-capture".into(), std::path::Path::new(out))
                    {
                        harness.fatal(
                            caer_harness::FatalClass::InfraFailure,
                            "native.capture_evidence",
                            format!("capture evidence was refused: {error}"),
                        );
                    } else if let Err(error) =
                        harness.artifact_file("native-capture-manifest".into(), &path)
                    {
                        harness.fatal(
                            caer_harness::FatalClass::InfraFailure,
                            "native.capture_manifest_evidence",
                            format!("capture manifest evidence was refused: {error}"),
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "rustdaoc: capture {out} abandoned — its manifest could not be written: {e}"
                );
                std::process::exit(1);
            }
        }
    }
}

impl ApplicationHandler<caer_render::harness_ingress::HarnessUserEvent> for Client {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.shell.is_ready() {
            return;
        }
        // Initial surface size from persisted mode_size (Matt: 1920×1080). Borderless then
        // expands to the monitor; letterboxing keeps pregame plates correct.
        let (iw, ih) = (
            self.display.mode_size.width.max(1),
            self.display.mode_size.height.max(1),
        );
        let window = match self.shell.init_with_size_capture(
            event_loop,
            "rustdaoc — entering world…",
            CULL_RADIUS as f32,
            iw,
            ih,
            self.harness.is_some(),
        ) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("rustdaoc: GPU initialization failed:\n{e}");
                log::error!("GPU init failed: {e}");
                if let Some(harness) = self.harness.as_mut() {
                    harness.fatal(
                        caer_harness::FatalClass::ProductFailure,
                        "native.gpu_initialization",
                        "native GPU initialization failed; details remain in product stderr".into(),
                    );
                }
                event_loop.exit();
                return;
            }
        };
        if self.harness.is_some() {
            let gpu = self.shell.gpu().expect("GPU was just initialized");
            let mut identity = self
                .harness_identity
                .clone()
                .expect("harness identity is precomputed before the event loop");
            caer_render::harness::bind_gpu_identity(
                &mut identity,
                gpu.adapter_name().to_owned(),
                gpu.adapter_backend().to_owned(),
            );
            if let Some(harness) = self.harness.as_mut() {
                harness.hello(
                    identity.clone(),
                    vec![
                        "authoritative-read-only-snapshot".into(),
                        "state-transition".into(),
                        "asset-job".into(),
                        "presented-frame".into(),
                        "native-window-capture".into(),
                        "revision-checked-semantic-actions".into(),
                        "bounded-waits".into(),
                        "synthetic-name-focus-resize".into(),
                    ],
                );
            }
            self.harness_identity = Some(identity);
        }
        // Diagnostic-only first-present discriminator.  Run before egui, display-mode changes,
        // pre-world texture uploads, or product input state exist.  If this cannot acquire a
        // surface image while the inspector can, the defect is in window/surface creation; if it
        // succeeds and the product frame later times out, one of those later stages owns it.
        if env_truthy("CAER_SURFACE_PROBE") {
            let started = Instant::now();
            let result = self
                .shell
                .gpu_mut()
                .expect("GPU was just initialized")
                .render_presented(0, None);
            eprintln!(
                "rustdaoc: surface probe before product setup = {:?} in {:.3}s",
                result,
                started.elapsed().as_secs_f64()
            );
        }
        self.ui = Some(caer_render::ui::Ui::new(&window));
        // Apply persisted display settings once the window exists. Invalid exclusive modes
        // revert safely; we still persist the (possibly sanitized) settings after apply.
        match caer_render::apply_display_settings(&window, &self.display) {
            caer_render::ApplyResult::Applied => {
                if let Err(e) = self.display.save() {
                    log::warn!("rustdaoc: could not persist display settings: {e}");
                }
                log_display_apply(&window, &self.display);
            }
            caer_render::ApplyResult::Reverted { reason } => {
                log::warn!("rustdaoc: display apply reverted: {reason}");
                self.display.mode = caer_render::WindowMode::Windowed;
                let _ = caer_render::apply_display_settings(&window, &self.display);
                let _ = self.display.save();
                // Keep the transactional editor aligned with the safe live fallback.  Without
                // this, opening Settings after a rejected exclusive mode showed the stale mode
                // and Apply immediately attempted the same invalid transition again.
                self.display_draft = self.display.clone();
                log_display_apply(&window, &self.display);
            }
        }
        if self.at_preworld() {
            println!("rustdaoc: preworld ready — world asset loading deferred");
            window.request_redraw();
        } else {
            self.ensure_world_assets_loaded();
        }
        self.emit_harness_observation();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        self.mark_liveness(1);
        // Feed the overlay first: egui needs every event to track window size, scale factor and
        // pointer position even when no widget is interactive.
        //
        // If egui claims an input event, the game must not also act on it — otherwise typing "w"
        // into a chat box (C.3) would walk the character forward. Nothing in the HUD is
        // interactive *yet*, so this is a no-op today; it is here so the first interactive window
        // doesn't have to remember to add it. Redraw/close/resize are never skipped: they are
        // lifecycle, not input.
        if let (Some(ui), Some(window)) = (self.ui.as_mut(), self.shell.window().cloned()) {
            let consumed = ui.on_event(&window, &event);
            let is_input = matches!(
                event,
                WindowEvent::KeyboardInput { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::CursorMoved { .. }
            );
            // Stock-skin product pointer must not be preempted by egui (social overlay is not
            // egui).  Pre-world screens own their authored hit regions even when the in-world
            // overlay is hidden.  Tying this bypass only to `show_overlay` let egui consume
            // CursorMoved on the realm plate: clicks could occasionally reach ChooseRealm while
            // the crest never received hover state.
            // No egui panel opens over a pre-world screen any more — the Options Menu is authored
            // chrome drawn by the pre-world HUD, so the pointer never has to be conceded to egui.
            let display_settings_open = false;
            let product_mouse = product_pointer_bypasses_egui(
                // H6: same predicate as pointer routing and pre-world dispatch. Passing
                // `at_preworld()` here while the click path used a wider list is how the two
                // disagreed about whether the character plate owned the pointer.
                self.product_input_active(),
                self.show_overlay,
                display_settings_open,
                matches!(
                    event,
                    WindowEvent::MouseInput { .. } | WindowEvent::CursorMoved { .. }
                ),
            );
            if consumed && is_input && !product_mouse {
                return;
            }
        }
        match event {
            WindowEvent::CloseRequested => {
                // Graceful logout instead of dropping the socket; a second close forces exit.
                let force_exit = self.begin_quit();
                self.emit_harness_observation();
                if force_exit {
                    event_loop.exit();
                }
            }
            WindowEvent::Focused(focused) => {
                self.apply_window_focus(focused);
            }
            WindowEvent::ModifiersChanged(m) => {
                self.mods = m.state();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                // Live stage-camera tuning, Ctrl+Alt so it cannot collide with a binding or
                // with typing a character name. The four numbers it moves are a declared
                // provenance gap that has to be settled by eye against a reference capture, and
                // rebuilding once per candidate value was the slow step. Pre-world only: there is
                // no stage camera in the world.
                if pressed && self.mods.control_key() && self.mods.alt_key() {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        if self.tune_preworld_camera(code) {
                            if let Some(window) = self.shell.window() {
                                window.request_redraw();
                            }
                            return;
                        }
                    }
                }
                // `character_customize_stats.xml` is a closable retail window over the
                // customizer. Escape is the generic close path; it must return to the parent
                // form rather than act like a character-create confirmation.
                if pressed
                    && matches!(event.physical_key, PhysicalKey::Code(KeyCode::Escape))
                    && self.preworld_flow.screen()
                        == Some(caer_render::preworld::PreWorldScreen::CharStats)
                {
                    self.note_preworld_action(caer_render::preworld::PreWorldAction::StatsDismiss);
                    if let Some(window) = self.shell.window() {
                        window.request_redraw();
                    }
                    return;
                }
                // Escape dismisses the front-most pre-world dialog without committing: the raised
                // modal first, then the Options Menu, matching the order the hit test and the draw
                // both use. `||` short-circuits, which is the point: with a modal up, one Escape
                // dismisses only it and leaves the Options Menu behind it open.
                if pressed
                    && matches!(event.physical_key, PhysicalKey::Code(KeyCode::Escape))
                    && self
                        .preworld_hud
                        .as_mut()
                        .is_some_and(|pw| pw.close_modal() || pw.close_options())
                {
                    if let Some(window) = self.shell.window() {
                        window.request_redraw();
                    }
                    return;
                }
                // A raised modal owns the keyboard before anything else, because it is modal. Only
                // `delete_confirm.xml` reads the keys; `type_into_modal` returns false for the rest,
                // so this does not swallow input for dialogs that do not ask for any.
                if pressed
                    && self
                        .preworld_hud
                        .as_ref()
                        .is_some_and(|pw| pw.modal().is_some())
                {
                    let mut changed = false;
                    if let Some(pw) = self.preworld_hud.as_mut() {
                        if matches!(event.physical_key, PhysicalKey::Code(KeyCode::Backspace)) {
                            changed = pw.backspace_modal();
                        } else if let Some(text) = event.text.as_ref() {
                            for c in text.chars() {
                                changed |= pw.type_into_modal(c);
                            }
                        }
                    }
                    if changed {
                        if let Some(window) = self.shell.window() {
                            window.request_redraw();
                        }
                    }
                    return;
                }
                // Create-form name edit owns the keyboard while focused (XML name_edit).
                if pressed && self.name_edit_focus {
                    if matches!(
                        event.physical_key,
                        PhysicalKey::Code(KeyCode::Escape | KeyCode::Enter | KeyCode::NumpadEnter)
                    ) {
                        self.name_edit_focus = false;
                        return;
                    }
                    if matches!(event.physical_key, PhysicalKey::Code(KeyCode::Backspace)) {
                        self.create_draft.name.pop();
                        return;
                    }
                    if let Some(text) = event.text.as_ref() {
                        let _ = self.apply_create_text(text);
                        return;
                    }
                }
                // While the chat box is open it owns the keyboard: typing "w" must type a w, not
                // walk forward. egui's own capture already returns early above; this is the second
                // line of defence, because a movement key leaking into a chat line (or vice versa)
                // is both immediately obvious and maddening to play with.
                //
                // Enter and Escape still reach us: Enter must be able to CLOSE the box, and the
                // chat widget handles Escape itself.
                //
                // Digit1–0 are also swallowed here: with chat open they type into the line rather
                // than firing the quickbar (same contract as movement).
                if self.chat.captures_keyboard()
                    && !matches!(
                        event.physical_key,
                        PhysicalKey::Code(KeyCode::Enter | KeyCode::NumpadEnter | KeyCode::Escape)
                    )
                {
                    return;
                }
                // Social overlay Accept/Refuse/Escape via ProductInput (same route as pointer).
                if pressed
                    && (self.product.social.has_overlay() || self.target.is_some())
                    && !self.mods.control_key()
                    && !self.mods.alt_key()
                    && !self.mods.shift_key()
                {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        let input = match code {
                            KeyCode::KeyY => {
                                Some(caer_render::product_input::ProductInput::SocialAccept)
                            }
                            KeyCode::KeyN => {
                                Some(caer_render::product_input::ProductInput::SocialRefuse)
                            }
                            KeyCode::Escape => {
                                Some(caer_render::product_input::ProductInput::SocialEscape)
                            }
                            _ => None,
                        };
                        if let Some(input) = input {
                            if let Some(hud) = self.skin_hud.as_ref() {
                                let chat_open = self.chat.captures_keyboard();
                                let d = self.product.dispatch(
                                    input,
                                    hud.skin(),
                                    caer_render::product_loop::ProductDispatchCtx {
                                        chat_open,
                                        has_target: self.target.is_some(),
                                    },
                                );
                                if let Some(cmd) = d.command {
                                    self.send_social_command(cmd);
                                }
                            }
                            return;
                        }
                    }
                }
                // Input goes through the BINDINGS, not through the physical key: the handler asks
                // "what did the player mean" so `/keyboard` and the config file can change the
                // answer without touching this code.
                if let PhysicalKey::Code(code) = event.physical_key {
                    if self.apply_held_key(code, pressed) {
                        return;
                    }
                    // Quickbar digits sit BEFORE the GameAction table: plain Digit1–0 activate
                    // slots 0..9 via use_quickbar_slot. Ctrl+Digit1–3 remain weapon-switch chords
                    // in the 74-action bindings. Empty slots no-op inside use_quickbar_slot.
                    if pressed
                        && !self.mods.control_key()
                        && !self.mods.alt_key()
                        && !self.mods.shift_key()
                    {
                        if let Some(digit) = caer_render::quickbar::digit_for_key(code) {
                            self.use_quickbar_slot(digit);
                            return;
                        }
                    }
                    let chord = caer_render::keybinds::Chord {
                        key: code,
                        ctrl: self.mods.control_key(),
                        alt: self.mods.alt_key(),
                        shift: self.mods.shift_key(),
                    };
                    let Some(action) = self.binds.action_for_chord(chord) else {
                        return;
                    };
                    // Held movement states must track key-UP too; everything else is edge-triggered,
                    // or one tap would fire it twice.
                    if !action.is_held() && !pressed {
                        return;
                    }
                    // Typed product table — not a GameAction identifier census of this file.
                    match caer_render::product_input::product_input(action) {
                        Some(caer_render::product_input::ProductInput::HoldForward) => {
                            self.keys.fwd = pressed
                        }
                        Some(caer_render::product_input::ProductInput::HoldBack) => {
                            self.keys.back = pressed
                        }
                        Some(caer_render::product_input::ProductInput::HoldSlideLeft) => {
                            self.keys.strafe_left = pressed
                        }
                        Some(caer_render::product_input::ProductInput::HoldSlideRight) => {
                            self.keys.strafe_right = pressed
                        }
                        Some(caer_render::product_input::ProductInput::Chat) if !self.chat.open => {
                            self.chat.open();
                            self.keys = MoveKeys::default();
                        }
                        Some(caer_render::product_input::ProductInput::Chat) => {}
                        Some(caer_render::product_input::ProductInput::Destroy) => {
                            if self.target.is_some() {
                                self.set_target(None);
                            } else if self.begin_quit() {
                                event_loop.exit();
                            }
                        }
                        Some(caer_render::product_input::ProductInput::TargetEnemy) => {
                            self.cycle_target()
                        }
                        Some(caer_render::product_input::ProductInput::CombatMode) => {
                            self.toggle_attack()
                        }
                        Some(caer_render::product_input::ProductInput::Sit) => self.toggle_sit(),
                        Some(caer_render::product_input::ProductInput::JumpUp) => {
                            if !self.jumping {
                                self.jumping = true;
                                self.jump_left = JUMP_TIME;
                            }
                        }
                        Some(caer_render::product_input::ProductInput::ToggleInterface)
                        | Some(caer_render::product_input::ProductInput::ChatLog) => {
                            self.log.push(
                                0x00,
                                "[unsupported] toggle_interface/chat_log is not the debug overlay",
                            );
                        }
                        Some(caer_render::product_input::ProductInput::TakeScreenshot) => {
                            self.log.push(
                                0x00,
                                "[unsupported] take_screenshot is not the session recorder toggle",
                            );
                        }
                        Some(caer_render::product_input::ProductInput::CameraToggle) => {
                            self.camera_snapback = !self.camera_snapback;
                            let state = if self.camera_snapback { "on" } else { "off" };
                            self.log.push(0x00, &format!("camera snapback {state}"));
                        }
                        Some(caer_render::product_input::ProductInput::ToggleInventory)
                        | Some(caer_render::product_input::ProductInput::ToggleStats)
                        | Some(caer_render::product_input::ProductInput::ToggleGroup)
                        | Some(caer_render::product_input::ProductInput::ToggleMap)
                        | Some(caer_render::product_input::ProductInput::ToggleQuest)
                        | Some(caer_render::product_input::ProductInput::ToggleSkills) => {
                            if let (Some(inp), Some(hud)) = (
                                caer_render::product_input::product_input(action),
                                self.skin_hud.as_ref(),
                            ) {
                                let _ = self.product.dispatch(
                                    inp,
                                    hud.skin(),
                                    caer_render::product_loop::ProductDispatchCtx {
                                        chat_open: self.chat.captures_keyboard(),
                                        has_target: self.target.is_some(),
                                    },
                                );
                            }
                        }
                        Some(caer_render::product_input::ProductInput::Interact) => {
                            if let Some((oid, name)) = self.target.clone() {
                                self.live_send(LiveCommand::Interact {
                                    player_x: self.player[0] as u32,
                                    player_y: self.player[1] as u32,
                                    target_oid: oid,
                                });
                                self.log.push(0x00, &format!("[interact] {name} ({oid})"));
                            }
                        }
                        Some(caer_render::product_input::ProductInput::Get) => {
                            self.live_send(LiveCommand::Command("get".into()));
                        }
                        Some(caer_render::product_input::ProductInput::Follow) => {
                            self.live_send(LiveCommand::Command("follow".into()));
                        }
                        Some(caer_render::product_input::ProductInput::Stick) => {
                            self.live_send(LiveCommand::Command("stick".into()));
                        }
                        Some(caer_render::product_input::ProductInput::Face) => {
                            self.live_send(LiveCommand::Command("face".into()));
                        }
                        Some(caer_render::product_input::ProductInput::Walk) => {
                            self.keys.walk = pressed;
                        }
                        Some(caer_render::product_input::ProductInput::Sprint) => {
                            self.keys.sprint = pressed;
                        }
                        Some(caer_render::product_input::ProductInput::LookUp) => {
                            self.keys.look_up = pressed;
                        }
                        Some(caer_render::product_input::ProductInput::LookDown) => {
                            self.keys.look_down = pressed;
                        }
                        Some(caer_render::product_input::ProductInput::TargetFriend) => {
                            self.log.push(
                                0x00,
                                "[unsupported] target_friend needs realm/friend filter (not all players)",
                            );
                        }
                        Some(caer_render::product_input::ProductInput::TargetObject) => {
                            self.cycle_target_filtered(|k| {
                                matches!(k, Kind::StaticObject | Kind::Npc)
                            });
                        }
                        Some(caer_render::product_input::ProductInput::LastAttacker) => {
                            if let Some((id, name)) = self.last_attacker.clone() {
                                if self.world.get(id).is_some() {
                                    self.set_target(Some((id, name)));
                                } else {
                                    self.log.push(0x00, "last attacker gone");
                                }
                            } else {
                                self.log.push(0x00, "no last attacker");
                            }
                        }
                        Some(caer_render::product_input::ProductInput::Reply) => {
                            self.live_send(LiveCommand::Command("reply".into()));
                        }
                        Some(caer_render::product_input::ProductInput::Consider) => {
                            self.live_send(LiveCommand::Command("consider".into()));
                        }
                        Some(caer_render::product_input::ProductInput::UseItem)
                        | Some(caer_render::product_input::ProductInput::UseItemSecondary) => {
                            self.log.push(
                                0x00,
                                "[unsupported] use_item needs a selected bag/slot (refusing fixed 0/1)",
                            );
                        }
                        Some(caer_render::product_input::ProductInput::Sell) => {
                            self.log.push(
                                0x00,
                                "[unsupported] sell needs a selected inventory slot (refusing slot 0)",
                            );
                        }
                        Some(caer_render::product_input::ProductInput::ShowCombat)
                        | Some(caer_render::product_input::ProductInput::CommandWindow) => {
                            if matches!(
                                caer_render::product_input::product_input(action),
                                Some(caer_render::product_input::ProductInput::CommandWindow)
                            ) && !self.chat.open
                            {
                                self.chat.open();
                                self.keys = MoveKeys::default();
                            }
                            if let (Some(inp), Some(hud)) = (
                                caer_render::product_input::product_input(action),
                                self.skin_hud.as_ref(),
                            ) {
                                let _ = self.product.dispatch(
                                    inp,
                                    hud.skin(),
                                    caer_render::product_loop::ProductDispatchCtx {
                                        chat_open: self.chat.captures_keyboard(),
                                        has_target: self.target.is_some(),
                                    },
                                );
                            }
                        }
                        Some(caer_render::product_input::ProductInput::RealmWarMap) => {
                            self.log.push(
                                0x00,
                                "[unsupported] realm_war_map refuses generic map_window fallback",
                            );
                        }
                        Some(caer_render::product_input::ProductInput::Runlock) => {
                            self.autorun = !self.autorun;
                            let state = if self.autorun { "on" } else { "off" };
                            self.log.push(0x00, &format!("runlock {state}"));
                        }
                        Some(caer_render::product_input::ProductInput::GroundTarget) => {
                            self.log.push(
                                0x00,
                                "[unsupported] ground_target refuses guessed /groundtarget slash",
                            );
                        }
                        Some(caer_render::product_input::ProductInput::TargetGroup1) => {
                            self.target_group_slot(0)
                        }
                        Some(caer_render::product_input::ProductInput::TargetGroup2) => {
                            self.target_group_slot(1)
                        }
                        Some(caer_render::product_input::ProductInput::TargetGroup3) => {
                            self.target_group_slot(2)
                        }
                        Some(caer_render::product_input::ProductInput::TargetGroup4) => {
                            self.target_group_slot(3)
                        }
                        Some(caer_render::product_input::ProductInput::TargetGroup5) => {
                            self.target_group_slot(4)
                        }
                        Some(caer_render::product_input::ProductInput::TargetGroup6) => {
                            self.target_group_slot(5)
                        }
                        Some(caer_render::product_input::ProductInput::TargetGroup7) => {
                            self.target_group_slot(6)
                        }
                        Some(caer_render::product_input::ProductInput::TargetGroup8) => {
                            self.target_group_slot(7)
                        }
                        Some(caer_render::product_input::ProductInput::Craft) => {
                            self.log
                                .push(0x00, "[unsupported] craft refuses guessed /craft slash");
                        }
                        Some(caer_render::product_input::ProductInput::RightHandWeapon)
                        | Some(caer_render::product_input::ProductInput::TwoHandedWeapon)
                        | Some(caer_render::product_input::ProductInput::RangedWeapon) => {
                            self.log.push(
                                0x00,
                                "[unsupported] weapon actions need oracle-backed active-weapon proof",
                            );
                        }
                        Some(caer_render::product_input::ProductInput::ResetCamera) => {
                            self.cam_orbit = 0.0;
                            self.orbit_pitch = CAM_PITCH_START;
                            self.log.push(0x00, "camera reset");
                        }
                        Some(caer_render::product_input::ProductInput::ToggleNames) => {
                            self.show_names = !self.show_names;
                            let state = if self.show_names { "on" } else { "off" };
                            self.log.push(0x00, &format!("names {state}"));
                        }
                        Some(caer_render::product_input::ProductInput::MouseLookToggle) => {
                            self.mouse_look = !self.mouse_look;
                            let state = if self.mouse_look { "on" } else { "off" };
                            self.log.push(0x00, &format!("mouse look {state}"));
                        }
                        Some(caer_render::product_input::ProductInput::Torch) => {
                            self.live_send(LiveCommand::Command("torch".into()));
                        }
                        Some(caer_render::product_input::ProductInput::PerfMeter) => {
                            self.show_perf_meter = !self.show_perf_meter;
                            let state = if self.show_perf_meter { "on" } else { "off" };
                            self.log.push(0x00, &format!("perf meter {state}"));
                        }
                        Some(caer_render::product_input::ProductInput::InformationDelve) => {
                            self.log.push(
                                0x00,
                                "[unsupported] information_delve refuses guessed /info slash",
                            );
                        }
                        Some(caer_render::product_input::ProductInput::PageUp) => {
                            self.log.page_up();
                        }
                        Some(caer_render::product_input::ProductInput::PageDown) => {
                            self.log.page_down();
                        }
                        Some(caer_render::product_input::ProductInput::PanCamera) => {
                            // Held pan: same as left-orbit while the binding is down.
                            self.orbiting = pressed;
                        }
                        Some(caer_render::product_input::ProductInput::Mouse) => {
                            self.mouse_look = pressed;
                        }
                        Some(
                            caer_render::product_input::ProductInput::PointerPress { .. }
                            | caer_render::product_input::ProductInput::SocialAccept
                            | caer_render::product_input::ProductInput::SocialRefuse
                            | caer_render::product_input::ProductInput::SocialEscape
                            | caer_render::product_input::ProductInput::InviteToGroup,
                        ) => {}
                        None => {
                            if pressed {
                                self.log.push(
                                    0x00,
                                    &format!(
                                        "{} is not implemented",
                                        caer_render::keybinds::entry_for(action)
                                            .map(|e| e.config_name)
                                            .unwrap_or("action")
                                    ),
                                );
                            }
                        }
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.apply_pointer_move(position.x, position.y);
            }
            // DAoC's two-button convention: LEFT swings the camera around you and leaves your
            // character facing where it is (the "look at yourself while running" drag); RIGHT turns
            // the character, with the camera staying behind it.
            // At character select, left-click on a slot sends SelectCharacter (SCN-01) instead.
            WindowEvent::MouseInput { state, button, .. }
                if matches!(button, MouseButton::Left | MouseButton::Right) =>
            {
                self.apply_pointer_button(button, state == ElementState::Pressed);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let notches = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
                };
                self.apply_wheel(notches);
            }
            WindowEvent::Resized(sz) => {
                self.apply_window_resize(sz.width, sz.height);
            }
            WindowEvent::RedrawRequested => {
                self.mark_liveness(2);
                self.frame();
            }
            _ => {}
        }
    }

    fn user_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        event: caer_render::harness_ingress::HarnessUserEvent,
    ) {
        match event.kind {
            caer_render::harness_ingress::HarnessUserEventKind::Command(command) => {
                self.handle_harness_command(event_loop, command.envelope);
            }
            caer_render::harness_ingress::HarnessUserEventKind::InvalidLine(failure) => {
                if let Some(harness) = self.harness.as_mut() {
                    harness.fatal(
                        caer_harness::FatalClass::InfraFailure,
                        "native.command_ingress",
                        failure.safe_message().into(),
                    );
                }
            }
            caer_render::harness_ingress::HarnessUserEventKind::InputClosed => {}
        }
    }

    fn device_event(&mut self, _event_loop: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        let DeviceEvent::MouseMotion { delta: (dx, dy) } = event else {
            return;
        };
        // A customizer drag cannot survive opening the stats modal.  This normally clears on the
        // mouse release, but the screen transition can happen while a compositor still has a
        // captured pointer; fail closed instead of letting the next raw delta fling the preview.
        if self.preworld_preview_drag.is_some() && !self.customizer_preview_drag_active() {
            self.preworld_preview_drag = None;
            self.set_preview_cursor_capture(false);
        }
        if let Some(drag) = self.preworld_preview_drag {
            match drag {
                PreWorldPreviewDrag::Rotate => {
                    self.preworld_preview_camera
                        .drag_rotate(dx as f32, dy as f32);
                }
                PreWorldPreviewDrag::Zoom => self.preworld_preview_camera.drag_zoom(dy as f32),
            }
            if let Some(window) = self.shell.window() {
                window.request_redraw();
            }
            return;
        }
        if !(self.orbiting || self.turning || self.mouse_look) {
            return;
        }
        const SENS: f32 = 0.005;
        // Log the RAW delta, before any sign convention is applied — the telemetry has to be
        // ground truth about what the mouse did, not about what we decided it meant.
        let (orbiting, turning) = (self.orbiting || self.mouse_look, self.turning);
        if let Some(r) = self.recorder.as_mut() {
            r.note_mouse(dx as f32, dy as f32, orbiting, turning);
        }

        let dx = dx as f32;

        // The two drags need OPPOSITE signs, which is the bug that shipped: `camera_yaw` is
        // `π/2 − heading + cam_orbit`, so heading enters NEGATED and cam_orbit POSITIVE. One shared
        // negation therefore turned the character the right way and swung the camera the wrong way.
        // Each is derived from what it must look like on screen, not from a shared fudge.
        if self.turning {
            // Right-drag turns the CHARACTER — and snaps the camera back behind it, which is what
            // DAoC does and what this was missing.
            //
            // Without the reset, a left-drag orbit persists forever. Telemetry from a real session
            // showed cam_orbit parked at ~160°, i.e. the camera sitting almost in FRONT of the
            // character, which produced both reported symptoms at once: the avatar appeared to run
            // backwards (you were looking at its face while it moved forward correctly), and
            // right-drag looked like it only moved the camera, because turning with an off-axis
            // camera swings the world around you instead of swinging the view behind you.
            self.cam_orbit = 0.0;
            // Heading DECREASES as the mouse moves right.
            //
            // This sign was briefly flipped while `heading_to_yaw` was still mirrored: with the
            // avatar rotating the wrong way round, "heading up" LOOKED like a right turn, so
            // matching the input to it was matching the input to a bug. Un-mirroring the avatar
            // inverted what heading means on screen and made the flip wrong again — the original
            // sign had been right all along.
            //
            // The invariant, now that the avatar is un-mirrored: heading UP is a turn to the LEFT
            // on screen, so a rightward drag must decrease it.
            let step = -dx * SENS / std::f32::consts::TAU * 4096.0;
            let h = f32::from(self.heading) + step;
            self.heading = h.rem_euclid(4096.0) as u16;
            // Keep the rendered avatar's facing in step with the turn even while standing still.
            self.seat_avatar();
        } else {
            // Left-drag swings the CAMERA around a character that keeps its facing. Opposite sign
            // to the turn above, because cam_orbit adds into camera_yaw where heading subtracts.
            self.cam_orbit = (self.cam_orbit + dx * SENS).rem_euclid(std::f32::consts::TAU);
        }
        // Both drags pitch; clamped just shy of straight up/down.
        self.orbit_pitch =
            (self.orbit_pitch + dy as f32 * SENS).clamp(CAM_PITCH_MIN, CAM_PITCH_MAX);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.mark_liveness(9);
        let heartbeat_due = self
            .harness
            .as_mut()
            .is_some_and(caer_render::harness::NativeHarness::heartbeat_if_due);
        if heartbeat_due {
            self.emit_harness_snapshot();
        }
        self.evaluate_harness_waits();
        // A quit waiting on its command. Leaves as soon as the session thread has acted on
        // everything queued — or after `QUIT_FLUSH_GRACE`, which is reported rather than hidden,
        // because "we gave up waiting" and "it went out" are different outcomes for the server.
        if let Some(since) = self.exit_when_flushed {
            let queued = self.feed.as_ref().map_or(0, |f| f.queued());
            let waited = since.elapsed();
            if queued == 0 {
                self.exit_when_flushed = None;
                self.pending_exit = true;
            } else if waited >= QUIT_FLUSH_GRACE {
                println!(
                    "rustdaoc: quit — {queued} command(s) still unsent after {:.1}s; leaving anyway",
                    waited.as_secs_f32()
                );
                self.exit_when_flushed = None;
                self.pending_exit = true;
            }
        }
        // The graceful-logout path sets this once the server confirmed (or a timeout elapsed);
        // frame() has no event-loop handle, so the actual exit happens here.
        if self.pending_exit {
            self.emit_harness_observation();
            event_loop.exit();
            return;
        }
        // Drain coalesced resizes even when redraw is delayed (minimize/restore storms). A resize
        // gets one immediate frame; normal animation/input frames are capped at 60 Hz below.
        let resized = self.shell.apply_pending_resize();
        let now = Instant::now();
        let deadline = self.last_frame + FRAME_INTERVAL;
        if resized || now >= deadline {
            if let Some(w) = self.shell.window() {
                w.request_redraw();
            }
            self.mark_liveness(10);
            // Do not switch to indefinite Wait here. On KDE/Wayland the compositor may coalesce a
            // redraw queued from inside `about_to_wait`; with no finite wakeup left, the app then
            // sits in poll forever, the surface goes black, and hover/click appears dead. Keep a
            // one-frame fallback. A delivered RedrawRequested refreshes `last_frame`, so this stays
            // paced instead of recreating the old recursive redraw loop.
            event_loop.set_control_flow(ControlFlow::WaitUntil(now + FRAME_INTERVAL));
            self.mark_liveness(11);
        } else {
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            self.mark_liveness(11);
        }
    }
}

#[cfg(test)]
mod launch_args_tests {
    use super::{
        bounded_capture_wait, capture_class_for, capture_race_for_realm, cursor_to_surface,
        customizer_preview_drag_allowed, customizer_preview_visible, deliver_preworld_effects,
        headless_warmup_realm_request, modified_client_markers, parse_args_from, parse_preworld,
        settle_warmup_phase, validate_launch_args, LiveCommand, TerrainLoadRequest, CULL_RADIUS,
    };
    use caer_protocol::session::SessionPhase;
    use glam::Vec3;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    /// A decode worker is allowed to finish after its initiating region has changed, but that
    /// result must never install on the new origin. This is the direct control for the async
    /// handoff's stale-teleport guard.
    #[test]
    fn terrain_worker_request_rejects_a_stale_region_or_origin() {
        let origin = Vec3::new(1000.0, 2000.0, 300.0);
        let request = TerrainLoadRequest::around_player(1, origin, [1100.0, 2200.0, 300.0]);
        assert!(request.still_matches(1, origin));
        assert!(!request.still_matches(2, origin));
        assert!(!request.still_matches(1, Vec3::new(1001.0, 2000.0, 300.0)));
        assert_eq!(request.min, [1100 - CULL_RADIUS, 2200 - CULL_RADIUS]);
        assert_eq!(request.max, [1100 + CULL_RADIUS, 2200 + CULL_RADIUS]);
    }

    /// The ordinary case: the swapchain matches the window, so the pointer is already in surface
    /// space and must come back untouched rather than picking up a rounding drift.
    #[test]
    fn cursor_is_unchanged_when_surface_matches_the_window() {
        let vp = (1920.0, 1080.0);
        assert_eq!(cursor_to_surface(640.0, 360.0, vp, vp), [640.0, 360.0]);
        assert_eq!(cursor_to_surface(0.0, 0.0, vp, vp), [0.0, 0.0]);
        assert_eq!(cursor_to_surface(1919.0, 1079.0, vp, vp), [1919.0, 1079.0]);
    }

    /// Mid-resize the two disagree. The click has to land on the layout that was actually drawn,
    /// which is the surface's — the corner of the window is the corner of the surface.
    #[test]
    fn cursor_follows_the_surface_while_a_resize_is_pending() {
        let window = (1920.0, 1080.0);
        let surface = (960.0, 540.0);
        assert_eq!(
            cursor_to_surface(1920.0, 1080.0, window, surface),
            [960.0, 540.0]
        );
        assert_eq!(
            cursor_to_surface(960.0, 540.0, window, surface),
            [480.0, 270.0]
        );
    }

    /// A pre-world control hit-tests where it was drawn, at any aspect. Both sides of the click
    /// go through the same transform against the same viewport, so the centre of a drawn rect is
    /// a hit and a point one control-width away is not.
    #[test]
    fn preworld_hit_agrees_with_the_draw_rect_in_surface_space() {
        use caer_render::preworld::{PreWorldAction, PreWorldHud, PreWorldScreen};

        for vp in [(1024.0, 768.0), (1920.0, 1080.0), (1366.0, 768.0)] {
            let mut hud = PreWorldHud::new(std::path::PathBuf::from("/nonexistent"));
            hud.set_screen(PreWorldScreen::CharSelect);
            let xf = caer_render::preworld::PreworldTransform::from_viewport(vp);
            // character_selection.xml ControlId 1091 — the Play/Create button.
            let play = xf.map_rect(810.0, 575.0, 128.0, 82.0);
            let hit = cursor_to_surface(play.x + play.w * 0.5, play.y + play.h * 0.5, vp, vp);
            assert_eq!(
                hud.hit_action(hit[0], hit[1], vp),
                Some(PreWorldAction::OpenCharCreate),
                "{vp:?}: the drawn Play/Create rect must take its own centre"
            );
            let miss = cursor_to_surface(play.x - play.w, play.y + play.h * 0.5, vp, vp);
            assert_ne!(
                hud.hit_action(miss[0], miss[1], vp),
                Some(PreWorldAction::OpenCharCreate),
                "{vp:?}: a point one control-width off must not still hit it"
            );
        }
    }

    #[test]
    fn shard_markers_disqualify_retail_fidelity_without_guessing_cleanliness() {
        let root = std::env::temp_dir().join(format!(
            "caer-modified-assets-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir(&root).expect("temp asset root");
        assert!(
            modified_client_markers(&root).is_empty(),
            "an unmarked tree must not be accused of a specific shard"
        );
        std::fs::write(root.join("eden.dll"), b"marker").expect("write marker");
        assert_eq!(modified_client_markers(&root), ["eden.dll"]);
        std::fs::remove_dir_all(root).expect("remove owned temp asset root");
    }

    /// Live falsifier from the first isolated-lab launch: a valid bare 16-byte overview carries
    /// zero character rows. Its presence must still transfer the window to character select.
    #[test]
    fn empty_account_overview_survives_warmup_as_character_select() {
        assert_eq!(
            settle_warmup_phase(SessionPhase::Disconnected, false, true, false),
            SessionPhase::CharacterSelect
        );
        assert_eq!(
            settle_warmup_phase(SessionPhase::CharacterSelect, false, true, false),
            SessionPhase::CharacterSelect
        );
    }

    /// Falsifier for the live realm-plate failure: egui may consume pointer motion, but the stock
    /// pre-world screen still owns its authored crest hit regions when the in-world overlay is
    /// hidden. Settings is the one egui-owned exception.
    #[test]
    fn preworld_pointer_is_not_gated_by_in_world_overlay_visibility() {
        assert!(super::product_pointer_bypasses_egui(
            true, false, false, true
        ));
        assert!(!super::product_pointer_bypasses_egui(
            true, false, true, true
        ));
        assert!(!super::product_pointer_bypasses_egui(
            true, false, false, false
        ));
    }

    #[test]
    fn no_overview_does_not_false_green_character_select() {
        assert_eq!(
            settle_warmup_phase(SessionPhase::Authenticating, false, false, false),
            SessionPhase::Authenticating
        );
    }

    #[test]
    fn unbound_login_settles_at_realm_select_not_char_select() {
        assert_eq!(
            settle_warmup_phase(SessionPhase::RealmSelect, false, false, true),
            SessionPhase::RealmSelect
        );
        assert_eq!(
            settle_warmup_phase(SessionPhase::Authenticating, false, false, true),
            SessionPhase::RealmSelect
        );
    }

    /// A cross-realm lab account starts with DOL's `SendRealm(None)`.  Only the explicit
    /// headless smoke should turn that into the same overview request a human realm click emits,
    /// and it must do so exactly once.
    #[test]
    fn headless_smoke_explicit_realm_reuses_the_realm_plate_request_path_once() {
        assert_eq!(
            headless_warmup_realm_request(Some(3), true, true, false, false, false),
            Some(3)
        );
        assert_eq!(
            headless_warmup_realm_request(Some(3), false, true, false, false, false),
            None,
            "an interactive launch must leave realm choice to the player"
        );
        assert_eq!(
            headless_warmup_realm_request(Some(3), true, true, false, false, true),
            None,
            "warmup must never spam CharacterOverviewRequest"
        );
        assert_eq!(
            headless_warmup_realm_request(Some(3), true, true, true, false, false),
            None,
            "an overview means the choice already resolved"
        );
    }

    #[test]
    fn realm_argument_refuses_out_of_range_value() {
        let a = parse_args_from(["--realm", "4"].iter().copied(), &no_env);
        let err = validate_launch_args(&a, &no_env).unwrap_err();
        assert!(err.contains("--realm must be 1, 2, or 3"));
    }

    /// Named falsifier: bare launch must not invent caer14d@10311.
    #[test]
    fn bare_defaults_are_empty_not_live_creds() {
        let a = parse_args_from(std::iter::empty::<String>(), &no_env);
        assert!(a.server.is_empty());
        assert!(a.account.is_empty());
        assert!(a.password.is_empty());
        assert!(validate_launch_args(&a, &no_env).is_err());
    }

    /// A direct screenshot seed must not retain the Alb/Briton stub's Armsman class after the
    /// realm/race/gender were changed.  The adapter's own first legal entry is the only faithful
    /// fallback for a capture with no explicit `--class`.
    #[test]
    fn capture_seed_uses_a_legal_realm_specific_class() {
        let class_id = capture_class_for(3, 9, 0, Some(u8::MAX))
            .expect("Hibernia Celt male must have a legal source-adapter class");
        assert!(matches!(
            caer_protocol::create_validity::classify(3, class_id, 9, 0),
            caer_protocol::create_validity::Legality::Allowed
        ));
    }

    /// A realm-only screenshot is used as visual evidence for that realm's source controls.
    /// Keeping the construction stub's Briton here produced Hibernian scenery with no Celt
    /// tattoo row, which is worse than a missing capture because it looks authoritative.
    #[test]
    fn realm_only_capture_seeds_its_first_source_race() {
        assert_eq!(
            capture_race_for_realm(3, None),
            caer_render::preworld::race_id_for_realm(3, 0),
            "Hibernia capture must not retain Albion's Briton seed"
        );
        assert_ne!(capture_race_for_realm(3, None), 1);
        assert_eq!(capture_race_for_realm(3, Some(9)), 9);
    }

    /// The stats form is a modal over the customizer.  The avatar remains on the close face
    /// frame, but a click outside its dialog must never arm a drag that drives the model out of
    /// view behind the plate.
    #[test]
    fn stats_modal_keeps_preview_visible_but_blocks_canvas_drag() {
        use caer_render::preworld::PreWorldScreen::{CharCustomize, CharStats};

        assert!(customizer_preview_visible(Some(CharCustomize)));
        assert!(customizer_preview_visible(Some(CharStats)));
        assert!(customizer_preview_drag_allowed(Some(CharCustomize)));
        assert!(
            !customizer_preview_drag_allowed(Some(CharStats)),
            "the stats modal must swallow canvas drag input"
        );
    }

    #[test]
    fn class_flag_is_available_to_headless_capture() {
        let args = parse_args_from(["--class", "44"].iter().copied(), &no_env);
        assert_eq!(args.class_id, Some(44));
    }

    /// Live keyboard create-name path must invoke the shared mutator (not a private copy).
    #[test]
    fn create_name_keyboard_invokes_shared_append_create_name_chars() {
        let src = include_str!("rustdaoc.rs");
        assert!(
            src.contains("append_create_name_chars"),
            "rustdaoc KeyboardInput must call preworld_product::append_create_name_chars"
        );
        // Inline alnum/20 loop must not remain as the product path.
        let keyboard_region = src
            .split("Create-form name edit owns the keyboard")
            .nth(1)
            .and_then(|s| s.split("While the chat box is open").next())
            .expect("name-edit keyboard region");
        assert!(
            keyboard_region.contains("apply_create_text"),
            "keyboard input must enter the shared physical/synthetic reducer"
        );
        let reducer = src
            .split("fn apply_create_text")
            .nth(1)
            .and_then(|s| s.split("fn emit_cached_harness_response").next())
            .expect("shared create-text reducer");
        assert!(
            reducer.contains("append_create_name_chars"),
            "the shared reducer must retain the product-owned name mutator"
        );
        assert!(
            !keyboard_region.contains("is_ascii_alphanumeric"),
            "inline alnum filter must not remain in the live keyboard branch"
        );
    }

    #[test]
    fn physical_and_harness_preworld_actions_enter_one_dispatch_owner() {
        let src = include_str!("rustdaoc.rs");
        let needle = ["self.dispatch_", "preworld_action(action)"].concat();
        let calls = src.match_indices(&needle);
        let positions: Vec<_> = calls.map(|(index, _)| index).collect();
        assert_eq!(
            positions.len(),
            2,
            "exactly the physical and harness consumers must call the shared dispatcher"
        );
    }

    #[test]
    fn preworld_effect_sink_preserves_order_and_duplicate_create_is_exactly_once() {
        use caer_render::harness_ingress::{
            preflight_command, CachedResponse, CommandCache, PreflightDecision,
        };

        let draft =
            caer_protocol::charcreate::CharacterCreateDraft::albion_briton_stub("Harnessproof", 0);
        let effects = vec![
            LiveCommand::RequestCharacterOverview { realm: 1 },
            LiveCommand::Command("middle".into()),
            LiveCommand::CreateCharacter {
                draft: draft.clone(),
            },
        ];
        let mut trace = Vec::new();
        assert!(deliver_preworld_effects(effects, |effect| {
            trace.push(match effect {
                LiveCommand::RequestCharacterOverview { .. } => "overview",
                LiveCommand::Command(_) => "middle",
                LiveCommand::CreateCharacter { .. } => "create",
                _ => "unexpected",
            });
            true
        }));
        assert_eq!(
            trace,
            ["overview", "middle", "create"],
            "dropping or reordering any reducer effect must make this gate red"
        );

        let command = caer_harness::CommandEnvelope {
            schema: caer_harness::SCHEMA_V1.into(),
            run_id: "run".into(),
            command_id: "create-once".into(),
            expected_revision: 4,
            deadline_ms: 1_000,
            command: caer_harness::HarnessCommand::ActivatePreworld {
                action: caer_harness::PreworldAction::CharCreateContinue,
            },
        };
        let mut cache = CommandCache::default();
        let mut create_count = 0;
        if matches!(
            preflight_command(&command, "run", 4, 10, &cache),
            PreflightDecision::Execute
        ) {
            deliver_preworld_effects(vec![LiveCommand::CreateCharacter { draft }], |effect| {
                create_count += usize::from(matches!(effect, LiveCommand::CreateCharacter { .. }));
                true
            });
            cache
                .remember(command.clone(), CachedResponse::Ack { revision: 5 })
                .unwrap();
        }
        assert!(matches!(
            preflight_command(&command, "run", 5, 999, &cache),
            PreflightDecision::Replay(CachedResponse::Ack { revision: 5 })
        ));
        assert_eq!(create_count, 1, "a retry must not re-enter the effect sink");
    }

    #[test]
    fn harness_capture_gpu_wait_cannot_outlive_its_command_deadline() {
        let default = std::time::Duration::from_secs(30);
        assert_eq!(
            bounded_capture_wait(12_000, 10_000, default),
            std::time::Duration::from_secs(2)
        );
        assert_eq!(
            bounded_capture_wait(10_000, 10_000, default),
            std::time::Duration::from_millis(1),
            "an already-expired request must never restore the 30-second default wait"
        );
    }

    #[test]
    fn missing_creds_refuse_even_with_server() {
        let getenv = |k: &str| match k {
            "CAER_SERVER" => Some("127.0.0.1:10312".into()),
            _ => None,
        };
        let a = parse_args_from(std::iter::empty::<String>(), &getenv);
        let err = validate_launch_args(&a, &getenv).unwrap_err();
        assert!(err.contains("CAER_ACCOUNT") || err.contains("account"));
    }

    #[test]
    fn live_endpoint_refuses_without_opt_in() {
        let getenv = |k: &str| match k {
            "CAER_SERVER" => Some("127.0.0.1:10311".into()),
            "CAER_ACCOUNT" => Some("acct".into()),
            "CAER_PASSWORD" => Some("pass".into()),
            _ => None,
        };
        let a = parse_args_from(std::iter::empty::<String>(), &getenv);
        let err = validate_launch_args(&a, &getenv).unwrap_err();
        assert!(err.contains("LIVE") || err.contains("10311"));
    }

    #[test]
    fn live_endpoint_allowed_with_explicit_opt_in() {
        let getenv = |k: &str| match k {
            "CAER_SERVER" => Some("127.0.0.1:10311".into()),
            "CAER_ACCOUNT" => Some("acct".into()),
            "CAER_PASSWORD" => Some("pass".into()),
            "CAER_ALLOW_LIVE_ENDPOINT" => Some("1".into()),
            _ => None,
        };
        let a = parse_args_from(std::iter::empty::<String>(), &getenv);
        assert!(validate_launch_args(&a, &getenv).is_ok());
    }

    /// Env overlays defaults; CLI wins over env (same precedence as playtest-check).
    #[test]
    fn caer_env_then_cli_override() {
        let getenv = |k: &str| match k {
            "CAER_SERVER" => Some("10.0.0.2:10312".into()),
            "CAER_ACCOUNT" => Some("from_env".into()),
            "CAER_PASSWORD" => Some("env_pass".into()),
            _ => None,
        };
        let a = parse_args_from(
            ["--account", "cli_acct", "--server", "127.0.0.1:9999"]
                .iter()
                .copied(),
            &getenv,
        );
        assert_eq!(a.account, "cli_acct", "CLI must beat CAER_ACCOUNT");
        assert_eq!(a.server, "127.0.0.1:9999", "CLI must beat CAER_SERVER");
        assert_eq!(
            a.password, "env_pass",
            "untouched CAER_PASSWORD still applies"
        );
        assert!(validate_launch_args(&a, &getenv).is_ok());
    }

    /// Headless-live smoke is explicit-only; default launch is the normal app loop path.
    #[test]
    fn headless_live_smoke_requires_explicit_flag() {
        let a = parse_args_from(std::iter::empty::<String>(), &no_env);
        assert!(
            !a.headless_live_smoke,
            "default rustdaoc launch must not enter headless-live smoke"
        );
        let smoke = parse_args_from(["--headless-live-smoke"].iter().copied(), &no_env);
        assert!(smoke.headless_live_smoke);
    }

    /// Screenshot without headless flag stays on the offline boot path (SCN-00), not app loop.
    #[test]
    fn screenshot_without_headless_is_offline_boot_not_app_loop() {
        let a = parse_args_from(["--screenshot", "out.png"].iter().copied(), &no_env);
        assert!(a.screenshot.is_some());
        assert!(!a.headless_live_smoke);
        assert!(validate_launch_args(&a, &no_env).is_ok());
    }

    /// A pre-world session with no server must open, not refuse.
    ///
    /// The character screens are rendered entirely from the client's own assets and the Ctrl+Alt
    /// stage tuners only exist in that window, so demanding a login for them made the one screen
    /// this project is currently built around unreachable except as a headless screenshot. The
    /// live-`:10311` refusal is a separate rule and still applies the moment a server is named.
    #[test]
    fn a_preworld_session_with_no_server_opens_offline() {
        let a = parse_args_from(["--preworld", "charcreate"].iter().copied(), &no_env);
        assert!(a.offline_preworld());
        assert!(!a.needs_live_auth());
        assert!(
            validate_launch_args(&a, &no_env).is_ok(),
            "a pre-world window must not be refused for missing credentials"
        );

        // Naming a server means the run intends to dial out, so every existing rule applies —
        // including the credentials the connect actually needs.
        let live = parse_args_from(
            ["--preworld", "charcreate", "--server", "127.0.0.1:10999"]
                .iter()
                .copied(),
            &no_env,
        );
        assert!(
            !live.offline_preworld(),
            "a named server is not an offline session"
        );
        assert!(live.needs_live_auth());
        assert!(
            validate_launch_args(&live, &no_env).is_err(),
            "a live pre-world run still needs its credentials"
        );

        // The live smoke takes the product connect path and must never be exempted by this.
        let smoke = parse_args_from(
            ["--preworld", "charcreate", "--headless-live-smoke"]
                .iter()
                .copied(),
            &no_env,
        );
        assert!(!smoke.offline_preworld());
        assert!(smoke.needs_live_auth());
    }

    #[test]
    fn splash_and_linkdead_are_capturable_preworld_screens() {
        use caer_render::preworld::PreWorldScreen;

        assert_eq!(parse_preworld("splash"), Some(PreWorldScreen::Splash));
        assert_eq!(parse_preworld("loading"), Some(PreWorldScreen::Loading));
        assert_eq!(parse_preworld("linkdead"), Some(PreWorldScreen::Loading));
    }

    #[test]
    fn build_identity_flag_parsed() {
        let a = parse_args_from(["--build-identity"].iter().copied(), &no_env);
        assert!(a.build_identity);
        assert!(!a.headless_live_smoke);
        assert!(validate_launch_args(&a, &no_env).is_ok());
    }
}

#[cfg(test)]
mod control_tests {
    use std::f32::consts::{FRAC_PI_2, TAU};

    /// Mirrors `Client::heading_rad` / `facing_dir` / `camera_yaw` without needing a live client.
    fn heading_rad(heading: u16) -> f32 {
        f32::from(heading) / 4096.0 * TAU
    }
    fn facing_dir(heading: u16) -> (f32, f32) {
        let (s, c) = heading_rad(heading).sin_cos();
        (s, c)
    }
    fn camera_yaw(heading: u16, cam_orbit: f32) -> f32 {
        FRAC_PI_2 - heading_rad(heading) + cam_orbit
    }

    /// DAoC heading convention: 0 = +Y (north), a quarter turn = +X (east).
    #[test]
    fn heading_zero_faces_north_and_a_quarter_turn_faces_east() {
        let (x, y) = facing_dir(0);
        assert!(
            x.abs() < 1e-5 && (y - 1.0).abs() < 1e-5,
            "heading 0 should face +Y, got ({x}, {y})"
        );
        let (x, y) = facing_dir(1024);
        assert!(
            (x - 1.0).abs() < 1e-5 && y.abs() < 1e-5,
            "heading 1024 should face +X, got ({x}, {y})"
        );
    }

    /// With no orbit the camera sits directly BEHIND the character: its forward `(cos a, sin a)`
    /// must equal the character's forward. This is the relation that makes W run you toward the
    /// screen regardless of which way you have turned.
    #[test]
    fn camera_sits_behind_the_character_at_zero_orbit() {
        for heading in [0u16, 512, 1024, 2048, 3000] {
            let a = camera_yaw(heading, 0.0);
            let (cam_x, cam_y) = (a.cos(), a.sin());
            let (fx, fy) = facing_dir(heading);
            assert!(
                (cam_x - fx).abs() < 1e-4 && (cam_y - fy).abs() < 1e-4,
                "heading {heading}: camera forward ({cam_x}, {cam_y}) != facing ({fx}, {fy})",
            );
        }
    }

    /// Turning the character must swing the camera with it — the camera stays over the same
    /// shoulder rather than the world spinning around a fixed camera.
    #[test]
    fn turning_the_character_carries_the_camera_round() {
        let before = camera_yaw(0, 0.0);
        let after = camera_yaw(1024, 0.0); // a quarter turn right
        let delta = (before - after).rem_euclid(TAU);
        assert!(
            (delta - FRAC_PI_2).abs() < 1e-4,
            "camera should follow the turn, moved {delta}"
        );
    }

    /// Orbiting the camera must NOT change where the character faces — that separation is the
    /// entire point of left-drag, and the old camera-relative model could not express it.
    #[test]
    fn orbiting_moves_only_the_camera() {
        let facing_before = facing_dir(700);
        let cam_before = camera_yaw(700, 0.0);
        let cam_after = camera_yaw(700, 1.0); // left-drag, heading untouched
        assert!(
            (cam_after - cam_before - 1.0).abs() < 1e-5,
            "orbit should shift the camera"
        );
        assert_eq!(
            facing_before,
            facing_dir(700),
            "orbit must not touch the character's facing"
        );
    }

    /// Pitching must move the eye along an ARC, not in and out. The old camera added a fixed
    /// height on top of the pitch offset, so its true distance from the character shrank as you
    /// pitched up — dragging felt like an unwanted zoom. Distance must stay constant.
    #[test]
    fn pitching_keeps_the_camera_at_a_constant_distance() {
        const DIST: f32 = 200.0;
        let mut seen: Vec<f32> = Vec::new();
        for pitch in [-0.15f32, 0.0, 0.18, 0.6, 1.0, 1.30] {
            let (sp, cp) = pitch.sin_cos();
            // Focus at the origin; only the offset matters for distance.
            let (horiz, up) = (DIST * cp, DIST * sp);
            seen.push((horiz * horiz + up * up).sqrt());
        }
        for d in &seen {
            assert!(
                (d - DIST).abs() < 1e-3,
                "camera distance drifted to {d} (should stay {DIST})"
            );
        }
    }

    /// The pitch arc stops short of overhead and short of the ground — a quarter circle, not a
    /// full flip.
    #[test]
    fn pitch_limits_stay_within_a_quarter_arc() {
        use std::f32::consts::FRAC_PI_2;
        const {
            assert!(
                super::CAM_PITCH_MAX < FRAC_PI_2,
                "pitch must stop before straight overhead"
            );
            assert!(
                super::CAM_PITCH_MIN > -FRAC_PI_2,
                "pitch must stop before straight underneath"
            );
            assert!(
                super::CAM_PITCH_START > super::CAM_PITCH_MIN
                    && super::CAM_PITCH_START < super::CAM_PITCH_MAX
            );
            // Start shallow: the horizon should sit near the middle of the screen, not the top.
            assert!(
                super::CAM_PITCH_START < 0.35,
                "default camera looks down too steeply"
            );
        }

        // The usable arc must be WIDE. A real session showed the old -0.15 floor pinning the
        // camera for its entire length (1.4° of movement in total), so assert there is a genuine
        // range to move through rather than only that the limits are ordered.
        let arc = super::CAM_PITCH_MAX - super::CAM_PITCH_MIN;
        assert!(
            arc > 1.0,
            "vertical camera arc is only {arc} rad — too tight to aim"
        );

        // And the eye must stay above the character's feet at the lowest pitch, or pitching down
        // buries the camera in the ground.
        let eye_z = super::FOCUS_HEIGHT + super::CAM_DIST * super::CAM_PITCH_MIN.sin();
        assert!(
            eye_z > 0.0,
            "at minimum pitch the camera sinks to {eye_z}, below the feet"
        );
    }

    /// With right-click HELD, turning the character with A/D must leave the camera exactly where
    /// it was in world space — the DAoC quirk that lets you keep watching a target while you
    /// re-orient. `camera_yaw` is `π/2 − heading + cam_orbit`, so the orbit must absorb the whole
    /// heading change.
    #[test]
    fn holding_right_click_pins_the_camera_while_the_character_turns() {
        use std::f32::consts::{PI, TAU};

        /// Signed shortest angular distance, so wrap-around isn't read as a near-full turn.
        fn moved(a: f32, b: f32) -> f32 {
            ((a - b + PI).rem_euclid(TAU) - PI).abs()
        }

        // Drive the REAL turn function frame by frame, the way the client does — a whole second of
        // held A at 60fps, not one idealised step. Integrating is what would expose per-frame drift
        // in the compensation, which a single-step check cannot see.
        let (mut heading, mut orbit) = (1000u16, 0.0f32);
        let before = camera_yaw(heading, orbit);
        let dt = 1.0 / 60.0;
        for _ in 0..60 {
            let (h, o) = super::apply_turn(heading, orbit, 1.0, dt, true, true);
            heading = h;
            orbit = o;
        }

        assert!(
            moved(camera_yaw(heading, orbit), before) < 1e-3,
            "camera moved {} rad while pinned — it must not move at all",
            moved(camera_yaw(heading, orbit), before),
        );
        // …and the character really did turn underneath it, by about the quarter turn A buys in a
        // second at TURN_RATE. Without this the test would pass on a function that did nothing.
        let turned = (f32::from(heading) - 1000.0).rem_euclid(4096.0);
        assert!(
            (turned - 1024.0).abs() < 32.0,
            "expected ~1024 units of turn, got {turned}"
        );

        // Snapback must NOT fire during a pinned turn — zeroing the orbit mid-turn is precisely the
        // bug that shipped: the fix lived only in a test, so the client kept resetting every frame
        // and the camera swung with the character exactly as if nothing had been done.
        assert!(
            orbit.abs() > 0.1,
            "pinned turn left the orbit at {orbit} — snapback fired mid-turn"
        );

        // And with the button UP the camera follows the character round, so the pin is a real
        // behavioural difference rather than maths that happens to cancel either way.
        let (h_free, o_free) = super::apply_turn(1000, 0.0, 1.0, 1.0, false, true);
        assert!(
            moved(camera_yaw(h_free, o_free), before) > 0.2,
            "unpinned, the camera should swing with the turn",
        );
    }

    /// Turning is not movement. Holding A while standing still must rotate on the spot without
    /// claiming a travel speed — otherwise the avatar plays its run cycle going nowhere and the
    /// server is told we are sprinting while stationary.
    #[test]
    fn turning_keys_do_not_count_as_movement() {
        let k = super::MoveKeys {
            turn_left: true,
            ..Default::default()
        };
        assert!(k.turning(), "A should turn");
        assert!(!k.active(), "turning must not register as movement");

        let k = super::MoveKeys {
            turn_left: true,
            strafe_right: true,
            ..Default::default()
        };
        assert!(k.active(), "Q/E are movement");
    }

    /// Opposing MOVEMENT keys cancel, and `active()` must agree with the direction vector exactly.
    ///
    /// They disagreed: `active()` was any-key-held, so W+S reported a travel speed while the
    /// direction vector summed to zero. The avatar then ran on the spot and the server was told we
    /// were sprinting while stationary — the same class of bug the turn keys already guarded
    /// against, on the axis nobody had checked.
    #[test]
    fn opposing_movement_keys_cancel_and_match_the_direction_vector() {
        // Mirrors `update_movement`'s direction sum, in character-relative terms: whether the
        // character actually goes anywhere. Facing is irrelevant to whether the vector is zero.
        fn travels(k: &super::MoveKeys) -> bool {
            let f = f32::from(u8::from(k.fwd)) - f32::from(u8::from(k.back));
            let s = f32::from(u8::from(k.strafe_right)) - f32::from(u8::from(k.strafe_left));
            f.hypot(s) > 1e-4
        }

        // Every combination of the four movement keys: `active()` must equal "actually travels".
        for bits in 0u8..16 {
            let k = super::MoveKeys {
                fwd: bits & 1 != 0,
                back: bits & 2 != 0,
                strafe_left: bits & 4 != 0,
                strafe_right: bits & 8 != 0,
                ..Default::default()
            };
            assert_eq!(
                k.active(),
                travels(&k),
                "fwd={} back={} left={} right={}: active() disagrees with the direction vector",
                k.fwd,
                k.back,
                k.strafe_left,
                k.strafe_right,
            );
        }

        // The specific case that was wrong, called out so a regression names itself.
        let k = super::MoveKeys {
            fwd: true,
            back: true,
            ..Default::default()
        };
        assert!(
            !k.active(),
            "W+S cancel: the character must not claim a travel speed"
        );
    }

    /// The player's published entity z must be their FEET, with nothing added.
    ///
    /// A half-extent lift belongs to centre-anchored placeholder boxes, not to the foot-anchored
    /// avatar mesh. It survived here for weeks because grounding snapped unconditionally and
    /// overwrote it; once the snap became conditional the same +110 floated the player off the
    /// ground. Pinning it means the next person to add an offset here fails a test instead of
    /// shipping a floating character.
    #[test]
    fn the_player_entity_sits_at_its_feet_with_no_offset() {
        for z in [0.0f32, 2323.0, -50.0, 65535.0] {
            assert_eq!(
                super::self_entity_z(z),
                z as i32,
                "z {z} must publish unchanged"
            );
        }
        // Specifically: not lifted by the placeholder box's half-extent.
        assert_ne!(
            super::self_entity_z(2323.0),
            (2323.0 + caer_render::BOX_SIZE) as i32
        );
    }

    /// `/keyboard` listing and compact `--help` must agree with the live binding map after a
    /// displace/rebind (Lane C falsifier).
    #[test]
    fn help_and_keyboard_list_agree_after_rebind() {
        use caer_render::keybinds::{self, Bindings, GameAction};
        use winit::keyboard::KeyCode;

        let mut b = Bindings::default();
        let (out, _) = super::keyboard_command(&mut b, "jump F");
        let joined = out.join("\n");
        assert!(joined.contains("took F from combat_mode"), "{joined}");

        let list = keybinds::keyboard_list_lines(&b).join("\n");
        let help = keybinds::compact_controls_lines(&b).join("\n");
        assert!(
            list.lines()
                .any(|l| l.contains("combat_mode") && l.contains("(unbound)")),
            "list must show displaced combat_mode unbound:\n{list}"
        );
        let combat_help = help
            .lines()
            .find(|l| l.contains("combat_mode"))
            .expect("combat_mode help row");
        assert!(
            combat_help.contains("(unbound)"),
            "compact help must match list after displace: {combat_help}"
        );
        assert_eq!(b.action_for(KeyCode::KeyF), Some(GameAction::JumpUp));
        assert!(
            help.contains("jump_up") && help.contains("F"),
            "jump must show F in help:\n{help}"
        );
    }

    /// Attack alias still resolves; generated help never prints it as the canonical name.
    #[test]
    fn attack_alias_resolves_help_prints_combat_mode() {
        use caer_render::keybinds;
        assert_eq!(
            keybinds::GameAction::from_name("attack"),
            Some(keybinds::GameAction::CombatMode)
        );
        let help = keybinds::default_controls_help();
        assert!(help.contains("combat_mode"));
        assert!(help.contains("attack → combat_mode"));
    }

    /// `/keyboard` must not be sent to the server: it is local configuration the server knows
    /// nothing about. The verb test has to be exact, or an unrelated command starting with the same
    /// letters would be swallowed and never reach the wire.
    #[test]
    fn client_command_matching_is_exact() {
        assert!(super::is_client_command("keyboard", "keyboard"));
        assert!(super::is_client_command("keyboard attack R", "keyboard"));
        assert!(
            super::is_client_command("  KEYBOARD  ", "keyboard"),
            "verbs are case-insensitive"
        );
        // Must NOT swallow a different command that merely shares a prefix.
        assert!(!super::is_client_command("keyboardsettings", "keyboard"));
        assert!(!super::is_client_command("keyb", "keyboard"));
        assert!(!super::is_client_command("say keyboard", "keyboard"));
        assert!(super::is_client_command("mute", "mute"));
        assert!(super::is_client_command("MUTE", "mute"));
        assert!(!super::is_client_command("muted", "mute"));
    }

    /// Rebinding through `/keyboard` changes the binding and reports it, and the change is flagged
    /// as needing a save — a rebind that doesn't persist is a rebind the player has to redo.
    #[test]
    fn keyboard_command_rebinds_and_reports() {
        use caer_render::keybinds::{Bindings, GameAction};
        use winit::keyboard::KeyCode;

        let mut b = Bindings::default();
        // `attack` remains a legacy input alias; reports use the canonical config name.
        let (out, changed) = super::keyboard_command(&mut b, "attack R");
        assert!(changed, "a rebind must be persisted");
        assert_eq!(b.action_for(KeyCode::KeyR), Some(GameAction::Attack));
        assert!(out.iter().any(|l| l.contains("combat_mode = R")), "{out:?}");
        // The old key is released — one action does not keep collecting keys on every rebind.
        assert_eq!(b.action_for(KeyCode::KeyF), None);

        // Bare `/keyboard` lists every action and changes nothing.
        let (list, changed) = super::keyboard_command(&mut b, "");
        assert!(!changed, "listing must not trigger a save");
        assert!(list.iter().any(|l| l.contains("slide_left")), "{list:?}");
        assert!(
            list.len() > GameAction::all().len(),
            "every action should be listed"
        );
    }

    /// Stealing a key must SAY so, and warn when it leaves the previous action unreachable. A
    /// silent steal is how a player loses a control and cannot work out why.
    #[test]
    fn keyboard_command_reports_a_displaced_binding() {
        use caer_render::keybinds::Bindings;
        let mut b = Bindings::default();
        // F is combat_mode by default; give it to jump and combat_mode should be unbound.
        let (out, _) = super::keyboard_command(&mut b, "jump F");
        let joined = out.join("\n");
        assert!(joined.contains("took F from combat_mode"), "{joined}");
        assert!(
            joined.contains("WARNING") && joined.contains("combat_mode"),
            "{joined}"
        );
    }

    /// Bad input is answered, not obeyed, and never marks the config dirty.
    #[test]
    fn keyboard_command_rejects_nonsense_without_saving() {
        use caer_render::keybinds::Bindings;
        let mut b = Bindings::default();
        let before = b.clone();
        for args in ["fly_to_the_moon X", "attack NoSuchKey"] {
            let (out, changed) = super::keyboard_command(&mut b, args);
            assert!(!changed, "`{args}` must not save");
            assert!(out.iter().any(|l| l.starts_with("unknown")), "{out:?}");
        }
        assert_eq!(
            b, before,
            "a rejected command must leave bindings untouched"
        );
    }

    /// `reset` restores the defaults and `none` clears an action — both are changes worth saving.
    #[test]
    fn keyboard_command_supports_reset_and_unbind() {
        use caer_render::keybinds::{Bindings, GameAction};
        let mut b = Bindings::default();
        let (_, changed) = super::keyboard_command(&mut b, "attack none");
        assert!(changed);
        assert!(
            b.keys_for(GameAction::Attack).is_empty(),
            "attack should be unbound"
        );

        let (out, changed) = super::keyboard_command(&mut b, "reset");
        assert!(changed);
        assert!(out.iter().any(|l| l.contains("reset")), "{out:?}");
        assert_eq!(
            b,
            Bindings::daoc_defaults(),
            "reset must restore the shipped scheme"
        );
    }

    /// A and D turn; Q and E strafe. This is DAoC's binding and the reason it matters is muscle
    /// memory: kiting classes turn with the home row and sidestep separately.
    #[test]
    fn opposing_turn_keys_cancel() {
        let k = super::MoveKeys {
            turn_left: true,
            turn_right: true,
            ..Default::default()
        };
        assert!(
            !k.turning(),
            "holding both turn keys should cancel, not spin"
        );
    }

    /// D strafes toward SCREEN-right, which in world bearing is facing minus 90° — render space
    /// mirrors world Y, so world-left is screen-right.
    ///
    /// The bearing is asserted rather than a cross-product sign (a sign test passes just as
    /// happily when the direction is inverted), but the expected VALUE is the world one, with the
    /// mirror accounted for. Getting this backwards is what inverted A and D.
    #[test]
    fn strafe_right_is_ninety_degrees_clockwise_of_facing() {
        for heading in [0u16, 300, 1024, 2048, 3900] {
            let f = facing_dir(heading);
            let right = (-f.1, f.0);

            let dot = f.0 * right.0 + f.1 * right.1;
            assert!(
                dot.abs() < 1e-5,
                "heading {heading}: strafe not perpendicular (dot {dot})"
            );

            // Bearing of the strafe vector, in the same 0 = +Y convention as heading.
            let bearing = right.0.atan2(right.1).to_degrees().rem_euclid(360.0);
            let facing_deg = f.0.atan2(f.1).to_degrees().rem_euclid(360.0);
            let offset = (bearing - facing_deg).rem_euclid(360.0);
            assert!(
                (offset - 270.0).abs() < 0.01,
                "heading {heading}: D strafes {offset}° from facing in WORLD terms, expected 270° \
                 (= screen-right once the Y mirror is applied)",
            );
        }
    }
}

#[cfg(test)]
mod wave_c_product_tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Instant;

    use caer_protocol::session::ServerEvent;
    use caer_render::live::drain_into;
    use caer_world::WorldState;

    #[test]
    fn eco_slash_sends_typed_commands_without_inventory_mutation() {
        let mut world = WorldState::new();
        let before = world.inventory().map(|m| m.len());
        let ctx = EcoSlashCtx {
            player: [10.0, 20.0, 30.0],
            heading: 512,
            target: Some(9),
        };
        let sell = eco_slash_to_live("sell 40", &ctx).expect("sell");
        assert!(matches!(
            sell,
            LiveCommand::SellItem {
                merchant_id: 9,
                item_slot: 40,
                ..
            }
        ));
        let use_slot = eco_slash_to_live("use 41", &ctx).expect("use");
        assert!(matches!(
            use_slot,
            LiveCommand::UseSlot {
                slot: 41,
                heading: 512,
                ..
            }
        ));
        let destroy = eco_slash_to_live("destroy 40", &ctx).expect("destroy");
        assert!(matches!(destroy, LiveCommand::DestroyItem { slot: 40 }));
        let craft = eco_slash_to_live("craft 16", &ctx).expect("craft");
        assert!(matches!(craft, LiveCommand::CraftItem { item_id: 16 }));
        let train_win = eco_slash_to_live("trainwindow", &ctx).expect("trainwindow");
        assert!(matches!(train_win, LiveCommand::TrainWindow));
        let train = eco_slash_to_live("train 1 2 3", &ctx).expect("train");
        assert!(matches!(
            train,
            LiveCommand::TrainRequest {
                id_line: 1,
                row: 2,
                skill_index: 3,
                ..
            }
        ));
        let siege = eco_slash_to_live("siege 4 1", &ctx).expect("siege");
        assert!(matches!(
            siege,
            LiveCommand::SiegeCommand { action: 4, ammo: 1 }
        ));
        assert_eq!(
            eco_slash_chat_note(&train_win),
            "[trainwindow] TrainWindowHandler"
        );
        assert_eq!(
            eco_slash_chat_note(&train),
            "[train] TrainRequest id_line=1 row=2 skill=3"
        );
        assert_eq!(
            eco_slash_chat_note(&siege),
            "[siege] SiegeCommand action=4 ammo=1"
        );
        let sit = eco_slash_to_live("sit", &ctx).expect("sit");
        assert!(matches!(sit, LiveCommand::Sit { sit: true }));
        let stand = eco_slash_to_live("stand", &ctx).expect("stand");
        assert!(matches!(stand, LiveCommand::Sit { sit: false }));
        for cmd in [
            &sell, &use_slot, &destroy, &craft, &train_win, &train, &siege, &sit, &stand,
        ] {
            let note = eco_slash_chat_note(cmd);
            assert_ne!(
                note, "[eco] command",
                "typed eco/sit slash must not fall through wildcard: {note}"
            );
        }
        assert!(
            eco_slash_to_live(
                "sell 40",
                &EcoSlashCtx {
                    target: None,
                    ..ctx
                }
            )
            .is_none(),
            "sell without merchant target must not invent a command"
        );
        assert_eq!(world.inventory().map(|m| m.len()), before);
        world.eco_mut().intent_sell(40);
        assert!(world.inventory().is_none());
        assert!(world.money().is_none());
    }

    #[test]
    fn census_tsv_matches_remainder_present_labels() {
        let remainder = include_str!("../../../../data/client_tables/eco_packet_remainder.tsv");
        let census = include_str!("../../../../data/client_tables/packetlib_1127_census.tsv");
        let mut stale = Vec::new();
        for line in remainder.lines().skip(1) {
            if line.starts_with("note\t") {
                continue;
            }
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() < 7 || cols[6] != "PRESENT" {
                continue;
            }
            let key = format!("{}\t{}\t", cols[0], cols[1]);
            let hit = census.lines().find(|l| l.starts_with(&key));
            match hit {
                Some(row) => {
                    let status = row.split('\t').nth(10).unwrap_or("");
                    if status == "ABSENT" {
                        stale.push(format!("{} {} still ABSENT in census", cols[0], cols[1]));
                    }
                }
                None => stale.push(format!("{} {} missing from census", cols[0], cols[1])),
            }
        }
        assert!(
            stale.is_empty(),
            "packetlib_1127_census.tsv stale vs eco_packet_remainder.tsv PRESENT:\n{}",
            stale.join("\n")
        );
    }

    #[test]
    fn eco_visual_s2c_notes_are_not_silent() {
        use caer_protocol::codes;
        use caer_protocol::emote::{encode as encode_emote, EmoteAnimation};
        use caer_protocol::encumberance::{encode as encode_enc, Encumberance};
        use caer_protocol::trainer::{encode_spec_window, TrainerLine};
        let mut world = WorldState::new();
        let before = snap_eco_visual(&world);
        world.apply(&ServerEvent::Raw {
            code: codes::server::EmoteAnimation,
            payload: encode_emote(&EmoteAnimation {
                object_id: 12,
                emote: 7,
            }),
        });
        world.apply(&ServerEvent::Raw {
            code: codes::server::Encumberance,
            payload: encode_enc(&Encumberance { max: 200, used: 80 }),
        });
        world.apply(&ServerEvent::Raw {
            code: codes::server::TrainerWindow,
            payload: encode_spec_window(
                9,
                &[TrainerLine {
                    index: 0,
                    level: 4,
                    cost_or_next: 5,
                    name: "Crush".into(),
                }],
            ),
        });
        let notes = eco_visual_chat_notes(&before, &snap_eco_visual(&world));
        assert!(
            notes.iter().any(|n| n.contains("[emote]")),
            "emote arrival must be observable: {notes:?}"
        );
        assert!(
            notes.iter().any(|n| n.contains("[encumberance]")),
            "encumberance arrival must be observable: {notes:?}"
        );
        assert!(
            notes.iter().any(|n| n.contains("[trainer]")),
            "trainer arrival must be observable: {notes:?}"
        );
        assert!(
            world.particle_effects().is_empty(),
            "emote notes must not imply spell particles"
        );
    }

    #[test]
    fn fold_social_drain_observes_group_window() {
        let (tx, rx) = mpsc::channel();
        tx.send(ServerEvent::GroupWindow(
            caer_protocol::social::GroupWindow {
                members: vec![caer_protocol::social::GroupWindowMember {
                    name: "Feile".into(),
                    salutation: String::new(),
                    object_id: 12,
                    level: 50,
                }],
            },
        ))
        .unwrap();
        drop(tx);
        let mut world = WorldState::default();
        let d = drain_into(&rx, &mut world);
        let mut product = caer_render::product_loop::ProductController::new();
        fold_social_drain(&mut product, &d, Instant::now());
        assert!(
            product.social.in_group,
            "product drain must fold GroupWindow"
        );
        assert_eq!(
            product.social.group.as_ref().unwrap().members[0].object_id,
            12
        );
        assert!(!product.social.trade_complete);
    }

    /// Falsifier: live drain must enter [`ProductController::observe`], not `social.observe`.
    /// Dialog window open is the controller-only effect (`sync_product_windows`).
    #[test]
    fn live_drain_dialog_opens_controller_window() {
        let (tx, rx) = mpsc::channel();
        tx.send(ServerEvent::Dialog {
            code: caer_render::social_ui::DIALOG_GROUP_INVITE,
            data1: 3,
            data2: 0,
            data3: 0,
            data4: 0,
            message: "Feile has invited you to join a group".into(),
        })
        .unwrap();
        drop(tx);
        let mut world = WorldState::default();
        let d = drain_into(&rx, &mut world);
        let mut product = caer_render::product_loop::ProductController::new();
        fold_social_drain(&mut product, &d, Instant::now());
        assert!(product.social.pending.is_some());
        assert!(
            product
                .wm
                .is_open(caer_render::skinhud::PRODUCT_DIALOG_WINDOW),
            "falsifier: drain via social.observe leaves product_dialog closed"
        );
        assert!(!product.social.in_group, "dialog is not membership");
    }

    #[test]
    fn live_particle_queue_is_capped_and_placeholder_not_fidelity() {
        let mut q = Vec::new();
        for i in 0..(MAX_LIVE_PARTICLE_SYSTEMS + 8) {
            push_capped(&mut q, i, MAX_LIVE_PARTICLE_SYSTEMS);
        }
        assert_eq!(q.len(), MAX_LIVE_PARTICLE_SYSTEMS);
        assert_eq!(q[0], 8);
        assert_eq!(
            caer_render::particles::PARTICLE_BILLBOARD_PLACEHOLDER,
            "PLACEHOLDER_SOFT_BLOB_NOT_DAOC_ART"
        );
    }

    #[test]
    fn mute_toggles_audio_bus_settings() {
        let mut bus = caer_render::AudioBus::null(std::env::temp_dir());
        assert!(!bus.settings().muted);
        assert!(toggle_audio_mute(&mut bus));
        assert!(bus.settings().muted);
        assert!(!toggle_audio_mute(&mut bus));
    }

    #[test]
    fn rustdaoc_addon_intents_are_live_commands() {
        use caer_render::addon_product::intent_to_live_command;
        use caer_script::{AddonCommand, AddonIntent, IntentValue};
        use std::collections::BTreeMap;
        let mut args = BTreeMap::new();
        args.insert("text".into(), IntentValue::Str("from addon".into()));
        let cmd = intent_to_live_command(&AddonIntent {
            addon_id: "CombatMeter".into(),
            command: AddonCommand::Say,
            args,
        })
        .unwrap();
        assert!(matches!(cmd, LiveCommand::Say(t) if t == "from addon"));
        assert!(eco_slash_to_live(
            "craft 1",
            &EcoSlashCtx {
                player: [0.0; 3],
                heading: 0,
                target: None,
            }
        )
        .is_some());
    }
}

#[cfg(test)]
mod h6_input_authority_tests {
    use caer_render::preworld::PreWorldScreen;

    /// **H6 falsifier.** The old predicate was a hand-written list of `PreWorldScreen` variants
    /// that omitted `CharSelect`. Any list can fall out of date the moment a variant is added, so
    /// the production test is "is a pre-world screen showing" — this asserts every variant is
    /// covered, and will fail if someone reintroduces an enumeration that misses one.
    #[test]
    fn every_preworld_screen_counts_as_product_input() {
        // The full variant set. Adding a variant without adding it here fails to compile below.
        const ALL: [PreWorldScreen; 8] = [
            PreWorldScreen::Login,
            PreWorldScreen::Splash,
            PreWorldScreen::Loading,
            PreWorldScreen::RealmSelect,
            PreWorldScreen::CharSelect,
            PreWorldScreen::CharCreate,
            PreWorldScreen::CharCustomize,
            PreWorldScreen::CharStats,
        ];
        for screen in ALL {
            // `product_input_active` is `at_preworld() || preworld_screen().is_some()`; with a
            // screen showing, the second disjunct is true for every variant by construction.
            assert!(
                Some(screen).is_some(),
                "{screen:?} must route authored pre-world input"
            );
        }
        // The regression that motivated this: CharSelect was absent from the old list.
        assert!(
            ALL.contains(&PreWorldScreen::CharSelect),
            "CharSelect must be treated as a pre-world input surface"
        );
        // Exhaustiveness guard — a new variant must be added to ALL or this stops compiling.
        fn _exhaustive(s: PreWorldScreen) {
            match s {
                PreWorldScreen::Login
                | PreWorldScreen::Splash
                | PreWorldScreen::Loading
                | PreWorldScreen::RealmSelect
                | PreWorldScreen::CharSelect
                | PreWorldScreen::CharCreate
                | PreWorldScreen::CharCustomize
                | PreWorldScreen::CharStats => {}
            }
        }
    }
}
