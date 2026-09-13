//! SCN-09 live capture harness — PlayerDeath 0xAE + PlayerRevive 0x89 from SoloDAoC.
//!
//! Drives GM `player kill self` then `player rez self` (oracle `SendPlayerDied` /
//! `SendPlayerRevive`). Records S2C GSTCP frames; when both codes appear, writes a sealed
//! source + candidate fixture under `CAER_DEATH_OUT` (default: `tmp/death_rt_capture`).
//!
//! **Does not claim Pass.** OWN_CAPTURE is only established after the written bytes are
//! installed under `fixtures/scn09_death/` with a sealed attestation `Fixture::load` accepts,
//! and the content hash is not `SCN09_CAER_AUTHORED_CAPTURE_SHA256`.
//!
//! ```bash
//! CAER_ACCOUNT=caer14d CAER_PASSWORD=caer14d CAER_SERVER=127.0.0.1:10311 \
//!   cargo \
//!   run -p caer-client --example death_rt
//! ```
//!
//! Wait ~20s after other RTs quit so link-death ghosts clear.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use caer_client::live_harness::{self, LiveCredentials, LiveExit};
use caer_client::{Closed, Config, LiveSession};
use caer_protocol::codes;
use caer_protocol::framing::ServerPacketHeader;
use caer_protocol::session::ServerEvent;
use caer_world::WorldState;

const DIR_S2C: u8 = 0x02;

#[derive(Default)]
struct Track {
    saw_death: bool,
    saw_revive: bool,
    death_oid: Option<u16>,
    death_killer: Option<u16>,
    revive_oid: Option<u16>,
    chats: Vec<String>,
}

fn apply(t: &mut Track, world: &mut WorldState, e: &ServerEvent) {
    match e {
        ServerEvent::PlayerDied(d) => {
            eprintln!(
                "death_rt: PlayerDied oid={} killer={}",
                d.object_id, d.killer_id
            );
            t.saw_death = true;
            t.death_oid = Some(d.object_id);
            t.death_killer = Some(d.killer_id);
        }
        ServerEvent::PlayerRevived(v) => {
            eprintln!("death_rt: PlayerRevived oid={}", v.object_id);
            t.saw_revive = true;
            t.revive_oid = Some(v.object_id);
        }
        ServerEvent::ChatMessage { text, .. } => t.chats.push(text.clone()),
        _ => {}
    }
    world.apply(e);
}

fn wait_until(
    sess: &mut LiveSession,
    t: &mut Track,
    world: &mut WorldState,
    secs: u64,
    pred: impl Fn(&Track) -> bool,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        match sess.poll() {
            Ok(evs) => {
                for e in &evs {
                    if let ServerEvent::ChatMessage { text, .. } = e {
                        eprintln!("death_rt: chat: {text}");
                    }
                    apply(t, world, e);
                }
                if pred(t) {
                    return true;
                }
            }
            Err(Closed(_)) => return pred(t),
        }
    }
    pred(t)
}

/// Split concatenated GSTCP frames; return (offset, length, code) for each complete frame.
fn frame_index(stream: &[u8]) -> Vec<(usize, usize, u8)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < stream.len() {
        match ServerPacketHeader::decode_prefix(&stream[i..]) {
            Ok(Some((h, _, used))) => {
                out.push((i, used, h.code));
                i += used;
            }
            _ => break,
        }
    }
    out
}

/// Build a replay-format capture: one S2C record per framed GSTCP slice.
fn wrap_replay_records(frames: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for raw in frames {
        out.push(DIR_S2C);
        out.extend_from_slice(&[0u8; 8]);
        out.extend_from_slice(&(raw.len() as u32).to_be_bytes());
        out.extend_from_slice(raw);
    }
    out
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::process::Command;
    // Prefer system sha256sum so we don't add a dep to the example.
    let tmp = std::env::temp_dir().join(format!("caer_death_rt_{}.bin", std::process::id()));
    let _ = fs::write(&tmp, bytes);
    let out = Command::new("sha256sum")
        .arg(&tmp)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    let _ = fs::remove_file(&tmp);
    out.split_whitespace()
        .next()
        .unwrap_or("HASH_FAILED")
        .to_string()
}

fn main() {
    let creds = LiveCredentials::from_env();
    let server = creds.server.clone();
    let account = creds.account.clone();
    let password = creds.password.clone();
    let out_dir = PathBuf::from(
        std::env::var("CAER_DEATH_OUT").unwrap_or_else(|_| "tmp/death_rt_capture".into()),
    );

    let mut cfg = Config::new(&server, &account, &password);
    cfg.auto_select = true;

    eprintln!(
        "death_rt: server={server} account={account} out={}",
        out_dir.display()
    );
    let mut sess = live_harness::connect(&cfg).unwrap_or_else(|e| live_harness::exit_connect(e));
    let mut track = Track::default();
    let mut world = WorldState::new();
    live_harness::wait_entered_world(&mut sess, live_harness::world_deadline(), |e| {
        apply(&mut track, &mut world, e)
    })
    .unwrap_or_else(|e| {
        LiveExit::from_wait_err(&e).bail(format!("FAIL world: {e}"));
    });
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(2), |e| {
        apply(&mut track, &mut world, e)
    });

    let self_oid = world
        .iter()
        .find(|e| e.kind == caer_world::Kind::Self_)
        .map(|e| e.object_id);
    eprintln!(
        "death_rt: in-world self_oid={:?} is_dead={:?}",
        self_oid,
        self_oid.map(|id| world.is_dead(id))
    );

    // Capture window: kill → death packet → rez → revive packet.
    sess.start_s2c_log();
    eprintln!("death_rt: GM player kill self");
    if let Err(e) = sess.command("player kill self") {
        eprintln!("FAIL command player kill self: {e}");
        std::process::exit(3);
    }
    if !wait_until(&mut sess, &mut track, &mut world, 15, |t| t.saw_death) {
        eprintln!("FAIL no PlayerDied 0xAE after player kill self (need GM priv?)");
        let log = sess.take_s2c_log();
        eprintln!("death_rt: captured {} S2C bytes before fail", log.len());
        live_harness::soft_quit(&mut sess, |e| apply(&mut track, &mut world, e));
        std::process::exit(1);
    }

    eprintln!("death_rt: GM player rez self");
    if let Err(e) = sess.command("player rez self") {
        eprintln!("FAIL command player rez self: {e}");
        std::process::exit(3);
    }
    if !wait_until(&mut sess, &mut track, &mut world, 15, |t| t.saw_revive) {
        eprintln!("FAIL no PlayerRevived 0x89 after player rez self");
        let log = sess.take_s2c_log();
        eprintln!("death_rt: captured {} S2C bytes before fail", log.len());
        live_harness::soft_quit(&mut sess, |e| apply(&mut track, &mut world, e));
        std::process::exit(1);
    }

    // Brief drain so any trailing frames land in the log.
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(1), |e| {
        apply(&mut track, &mut world, e)
    });
    let s2c = sess.take_s2c_log();
    live_harness::soft_quit(&mut sess, |e| apply(&mut track, &mut world, e));

    let frames = frame_index(&s2c);
    let ae_meta: Vec<(usize, usize)> = frames
        .iter()
        .filter(|(_, _, c)| *c == codes::server::PlayerDeath)
        .map(|(off, len, _)| (*off, *len))
        .collect();
    let rv_meta: Vec<(usize, usize)> = frames
        .iter()
        .filter(|(_, _, c)| *c == codes::server::PlayerRevive)
        .map(|(off, len, _)| (*off, *len))
        .collect();

    eprintln!(
        "death_rt: S2C log {} B / {} frames; 0xAE×{} 0x89×{}",
        s2c.len(),
        frames.len(),
        ae_meta.len(),
        rv_meta.len()
    );

    if ae_meta.is_empty() || rv_meta.is_empty() {
        eprintln!(
            "FAIL raw frame index missing 0xAE or 0x89 (events decoded but frames not logged?)"
        );
        std::process::exit(1);
    }

    let (ae_off, ae_len) = ae_meta[0];
    let (rv_off, rv_len) = rv_meta[0];
    let ae_frame = &s2c[ae_off..ae_off + ae_len];
    let rv_frame = &s2c[rv_off..rv_off + rv_len];

    // Fixture wants exactly 1× each — take first of each from the live window.
    let capture = wrap_replay_records(&[ae_frame, rv_frame]);
    let capture_sha = sha256_hex(&capture);

    // Sealed source = capture.bin (identity derivation). Full S2C window kept for audit.
    let sealed = capture.clone();
    let sealed_sha = sha256_hex(&sealed);
    let source_sha = sha256_hex(&s2c);

    fs::create_dir_all(&out_dir).unwrap_or_else(|e| {
        eprintln!("FAIL mkdir {}: {e}", out_dir.display());
        std::process::exit(2);
    });
    let sealed_path = out_dir.join("sealed_ae89_replay.bin");
    let full_path = out_dir.join("s2c_window_gstcp.bin");
    let capture_path = out_dir.join("capture.bin");
    let manifest_path = out_dir.join("manifest.toml.candidate");
    fs::write(&sealed_path, &sealed).unwrap();
    fs::write(&full_path, &s2c).unwrap();
    fs::write(&capture_path, &capture).unwrap();

    let notes = format!(
        "OWN_CAPTURE candidate from SoloDAoC live session account={account} server={server}. \
         GM `player kill self` → SendPlayerDied 0xAE; GM `player rez self` → SendPlayerRevive 0x89. \
         victim={:?} killer={:?} revive={:?}. Full S2C window also at s2c_window_gstcp.bin \
         (sha256={source_sha}; ae_off={ae_off} ae_len={ae_len} rv_off={rv_off} rv_len={rv_len}). \
         Not Pass until installed under fixtures/scn09_death with Fixture::load green.",
        track.death_oid, track.death_killer, track.revive_oid
    );

    let manifest = format!(
        "id = \"scn09_death\"\n\
         provenance = \"OWN_CAPTURE\"\n\
         capture = \"capture.bin\"\n\
         content_sha256 = \"{capture_sha}\"\n\
         source_capture = \"crates/caer-capture/tests/fixtures/rustdaoc_death_ae89.bin\"\n\
         source_sha256 = \"{sealed_sha}\"\n\
         derivation = \"identity\"\n\
         notes = \"{notes}\"\n"
    );
    fs::write(&manifest_path, manifest).unwrap();

    eprintln!("death_rt: wrote {}", sealed_path.display());
    eprintln!("death_rt: wrote {}", full_path.display());
    eprintln!("death_rt: wrote {}", capture_path.display());
    eprintln!("death_rt: wrote {}", manifest_path.display());
    eprintln!("death_rt: capture_sha256={capture_sha}");
    eprintln!("death_rt: sealed_sha256={sealed_sha}");
    eprintln!(
        "death_rt: CAPTURE_OK death_oid={:?} killer={:?} revive={:?} — install under fixtures before claiming ProtocolSlice",
        track.death_oid, track.death_killer, track.revive_oid
    );
    // Exit 0 = harness observed live 0xAE+0x89 and wrote candidates. Not SCN-09 Pass.
    std::process::exit(0);
}
