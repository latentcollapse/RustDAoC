//! `caer-client` — a headless DAoC client, the P1 proof.
//!
//! Connects to a real DoL server over TCP and drives the login → character-select → world-entry
//! sequence entirely from `caer-protocol` (the verified session machine + encoders). No rendering,
//! no Wine, no original game.dll — just our Rust protocol stack *being* the client.
//!
//! The session-driving loop itself now lives in this crate's library ([`caer_client::LiveSession`]),
//! shared with `caer-render --live` so there is ONE live-connection loop, not two. This bin is the
//! thin CLI over it: it consumes the [`ServerEvent`] stream for human-readable logging + the P1
//! success criteria, and issues the one-shot `--say` / `--walk` after entry.
//!
//! Usage:
//!   caer-client --server 127.0.0.1:10311 --account rustdaoc --password rustdaoc [--char NAME]
//!               [--stay SECS] [--say MSG] [--walk UNITS]

use std::time::{Duration, Instant};

use caer_client::{Closed, Config, LiveSession};
use caer_protocol::session::ServerEvent;

struct Args {
    server: String,
    account: String,
    password: String,
    /// Character to play, by name (default: the first character on the overview).
    character: Option<String>,
    /// Seconds to remain in-world after entry, keepalive-pinging (0 = leave immediately).
    stay: u64,
    /// A line to /say once in-world (visible to nearby players — the on-screen proof).
    say: Option<String>,
    /// Game units to walk along +y once in-world (0 = stand still). Position persists on
    /// logout, so a second run shows the moved spawn — movement is self-verifiable.
    walk: f32,
    /// Log out cleanly with `/quit` (wait for the server's Quit confirmation) instead of just
    /// dropping the socket. Proves the no-link-death-ghost path: a re-login right after succeeds.
    quit: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut server = "127.0.0.1:10311".to_string();
    let mut account = "caerbot".to_string();
    let mut password = "caerbot".to_string();
    let mut character = None;
    let mut stay = 0u64;
    let mut say = None;
    let mut walk = 0f32;
    let mut quit = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--server" => server = it.next().ok_or("--server needs a value")?,
            "--account" => account = it.next().ok_or("--account needs a value")?,
            "--password" => password = it.next().ok_or("--password needs a value")?,
            "--char" => character = Some(it.next().ok_or("--char needs a value")?),
            "--stay" => {
                stay = it.next().ok_or("--stay needs seconds")?.parse().map_err(|_| "--stay needs a number of seconds")?
            }
            "--say" => say = Some(it.next().ok_or("--say needs a message")?),
            "--walk" => {
                walk = it.next().ok_or("--walk needs game units")?.parse().map_err(|_| "--walk needs a number")?
            }
            "--quit" => quit = true,
            "-h" | "--help" => {
                return Err(
                    "usage: caer-client --server host:port --account A --password P [--char NAME] [--stay SECS] [--say MSG] [--walk UNITS] [--quit]"
                        .into(),
                )
            }
            other => return Err(format!("unknown arg: {other}")),
        }
    }
    Ok(Args {
        server,
        account,
        password,
        character,
        stay,
        say,
        walk,
        quit,
    })
}

fn log(msg: &str) {
    println!("\x1b[36m[caer-client]\x1b[0m {msg}");
}

/// How long login → world-entry may stall (no decoded events) before we give up. In-world, the
/// keepalive keeps events flowing; pre-entry silence this long means the sequence failed.
const STALL_LIMIT: Duration = Duration::from_secs(10);

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    log(&format!(
        "connecting to {} as '{}'…",
        args.server, args.account
    ));
    let mut cfg = Config::new(&args.server, &args.account, &args.password);
    cfg.character = args.character.clone();
    let mut sess = match LiveSession::connect(&cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("connect failed: {e}");
            std::process::exit(1);
        }
    };

    // P1 success flags + what the bot saw, accumulated from the event stream.
    let mut got_login = false;
    let mut got_overview = false;
    let mut got_world = false;
    let mut position: Option<(f32, f32, f32)> = None;
    let mut npcs: Vec<caer_protocol::entities::Npc> = Vec::new();
    let mut objects: Vec<caer_protocol::entities::StaticObject> = Vec::new();
    let mut entity_updates = 0usize;
    // Last vitals we printed, so repeats of an unchanged status stay quiet.
    let mut last_status: Option<caer_protocol::status::PlayerStatus> = None;
    // Players currently in view, so a removal can name who left rather than printing a bare id.
    let mut players: std::collections::HashMap<u16, String> = std::collections::HashMap::new();
    let mut removals = 0usize;
    // The player's usable skills, accumulated across VariousUpdate pages.
    let mut skills: Vec<caer_protocol::skills::Skill> = Vec::new();
    let mut raw_counts: std::collections::HashMap<u8, usize> = std::collections::HashMap::new();

    // Post-entry one-shots (say/walk/stay) fire exactly once, the first loop after world entry.
    let mut acted = false;
    let mut stay_until: Option<Instant> = None;
    // Set once `/quit` has been sent; we keep looping (pinging) until the server confirms.
    let mut quit_deadline: Option<Instant> = None;
    // Give-up clock: reset on every decoded event; trips only if login/entry stalls silently.
    let mut last_progress = Instant::now();
    let mut say = args.say.clone();

    'session: loop {
        let events = match sess.poll() {
            Ok(ev) => ev,
            Err(Closed(reason)) => {
                log(&reason);
                break;
            }
        };
        if !events.is_empty() {
            last_progress = Instant::now();
        }
        for ev in &events {
            match ev {
                ServerEvent::EquipmentUpdated(e) => log(&format!(
                    "← equipment for {}: {} visible item(s)",
                    e.object_id,
                    e.items.len()
                )),
                ServerEvent::CharacterSheet(sh) => log(&format!(
                    "← character sheet: {} lvl {} ({}), realm rank {} — {}",
                    sh.name, sh.level, sh.class_name, sh.realm_level, sh.realm_rank_title
                )),
                ServerEvent::CryptKeyReceived => {
                    log("← server sent crypt key + version; sending login →")
                }
                ServerEvent::LoginGranted => {
                    log("\x1b[32m✓ LOGIN GRANTED\x1b[0m — driving char-select + session assignment →");
                    got_login = true;
                }
                ServerEvent::SessionAssigned(id) => {
                    log(&format!("\x1b[32m✓ SESSION ID {id}\x1b[0m assigned — requesting character overview →"));
                }
                ServerEvent::CharacterOverview(ov) => {
                    log("\x1b[32m✓ CHARACTER OVERVIEW received and decoded\x1b[0m — the character-select screen:");
                    if ov.characters.is_empty() {
                        log("    (no characters on this account/realm yet)");
                    }
                    for c in &ov.characters {
                        log(&format!(
                            "    slot {}: {} — level {} {} {} ({})",
                            c.slot, c.name, c.level, c.race_name, c.class_name, c.location
                        ));
                    }
                    got_overview = true;
                    match sess.chosen_character() {
                        Some((slot, name)) => log(&format!(
                            "→ selecting '{name}' (slot {slot}) — char select + region list →"
                        )),
                        None => {
                            if let Some(want) = &args.character {
                                log(&format!("    requested character '{want}' not on this overview — staying at char select"));
                            }
                        }
                    }
                }
                ServerEvent::RegionHandoff { ip, port } => {
                    log(&format!("\x1b[32m✓ REGION HANDOFF\x1b[0m — server points at {ip}:{port} (same socket on DoL); driving WorldInit/PlayerInit/GameOpen →"));
                }
                ServerEvent::EnteredWorld => {
                    log("\x1b[32m✓ ENTERED WORLD\x1b[0m — server sent CharacterInitFinished");
                    got_world = true;
                    log(&format!(
                        "\x1b[32m✓ THE BOT SEES\x1b[0m {} NPCs and {} objects in the visible area{}",
                        npcs.len(),
                        objects.len(),
                        if npcs.is_empty() { "" } else { "; nearest:" }
                    ));
                    if let Some((px, py, _)) = position {
                        let mut by_dist: Vec<(f32, &caer_protocol::entities::Npc)> = npcs
                            .iter()
                            .map(|n| {
                                let (dx, dy) = (n.x as f32 - px, n.y as f32 - py);
                                ((dx * dx + dy * dy).sqrt(), n)
                            })
                            .collect();
                        by_dist.sort_by(|a, b| a.0.total_cmp(&b.0));
                        for (dist, n) in by_dist.iter().take(8) {
                            log(&format!(
                                "    {:>5.0} u  {} \x1b[90m<{}>\x1b[0m  lvl {}  at ({}, {}, {})",
                                dist, n.name, n.guild, n.level, n.x, n.y, n.z
                            ));
                        }
                    }
                }
                ServerEvent::ChatMessage { chat_type, text } => {
                    log(&format!("  \x1b[33m[chat t{chat_type}]\x1b[0m {text}"));
                }
                ServerEvent::PlayerPosition {
                    x,
                    y,
                    z,
                    object_id,
                    heading,
                } => {
                    log(&format!("\x1b[32m✓ SPAWN POSITION\x1b[0m ({x:.0}, {y:.0}, {z:.0}) heading 0x{heading:04x}, object id {object_id}"));
                    position = Some((*x, *y, *z));
                }
                ServerEvent::NpcInView(npc) => npcs.push(npc.clone()),
                ServerEvent::ObjectInView(obj) => objects.push(obj.clone()),
                ServerEvent::EntityUpdated(_) => entity_updates += 1,
                // The skill list arrives in pages; report it once assembled rather than per page.
                ServerEvent::SkillsPage(page) => {
                    caer_protocol::skills::apply_page(&mut skills, page.clone());
                    log(&format!(
                        "\x1b[32m✓ SKILLS\x1b[0m {} usable entries (page at index {})",
                        skills.len(),
                        page.first_index,
                    ));
                }
                // Another player — worth a line each, they are rare and interesting.
                ServerEvent::PlayerInView(p) => {
                    log(&format!(
                        "\x1b[32m✓ PLAYER IN VIEW\x1b[0m {} <{}> — level {}, realm {}",
                        p.name,
                        if p.guild.is_empty() {
                            "no guild"
                        } else {
                            &p.guild
                        },
                        p.level,
                        p.realm,
                    ));
                    players.insert(p.object_id, p.name.clone());
                }
                // High-frequency and uninteresting individually; counted, and named only when it
                // is a player leaving (which is the one a human cares about).
                ServerEvent::ObjectRemoved { object_id } => {
                    removals += 1;
                    if let Some(name) = players.remove(object_id) {
                        log(&format!("  {name} left view"));
                    }
                }
                // Our own vitals. The server repeats these constantly (hundreds per session), so
                // only an actual change is worth a line — otherwise it drowns the transcript.
                ServerEvent::StatusUpdate(s) => {
                    if last_status != Some(*s) {
                        log(&format!(
                            "  vitals: {}/{} hp ({}%) · {}/{} power · {}/{} end{}",
                            s.health,
                            s.max_health,
                            s.health_pct,
                            s.mana,
                            s.max_mana,
                            s.endurance,
                            s.max_endurance,
                            if s.sitting { " · sitting" } else { "" },
                        ));
                        last_status = Some(*s);
                    }
                }
                ServerEvent::LoggedOut { total_out, level } => {
                    log(&format!("\x1b[32m✓ LOGGED OUT\x1b[0m (level {level}, total_out {total_out}) — server saved + removed us"));
                }
                ServerEvent::CombatAnimation(a) => {
                    log(&format!(
                        "  combat 0xBC: {} → {} result={} hp%={}",
                        a.attacker_id,
                        a.defender_id,
                        a.result.label(),
                        a.target_health_pct
                    ));
                }
                ServerEvent::InventoryUpdated(u) => {
                    log(&format!(
                        "  inventory 0x02: {} slot entr(y/ies)",
                        u.items.len()
                    ));
                }
                ServerEvent::MoneyUpdated(m) => {
                    log(&format!(
                        "  money 0xFA: {}g {}s {}c",
                        m.gold, m.silver, m.copper
                    ));
                }
                ServerEvent::PlayerDied(d) => {
                    log(&format!(
                        "  death 0xAE: oid={} killer={}",
                        d.object_id, d.killer_id
                    ));
                }
                ServerEvent::PlayerRevived(v) => {
                    log(&format!("  revive 0x89: oid={}", v.object_id));
                }
                ServerEvent::MerchantWindow(w) => {
                    log(&format!(
                        "  merchant 0x17: page {} — {} offer(s)",
                        w.page,
                        w.items.len()
                    ));
                }
                ServerEvent::SpellCast(c) => {
                    log(&format!(
                        "  spell cast 0x72: caster={} spell={} time={}",
                        c.caster_id, c.spell_id, c.cast_time
                    ));
                }
                ServerEvent::SpellEffect(e) => {
                    log(&format!(
                        "  spell effect 0x1B: caster={} spell={} target={} success={}",
                        e.caster_id, e.spell_id, e.target_id, e.success
                    ));
                }
                ServerEvent::SpellInterrupted(i) => {
                    log(&format!("  spell interrupt 0x73: oid={}", i.object_id));
                }
                ServerEvent::RegionChanged(r) => {
                    log(&format!(
                        "\x1b[32m✓ REGION CHANGED\x1b[0m → region {} (0xB7)",
                        r.region_id
                    ));
                }
                ServerEvent::Raw { code, payload } => {
                    let n = raw_counts.entry(*code).or_insert(0usize);
                    *n += 1;
                    if *n == 1 {
                        log(&format!(
                            "  (undecoded s2c 0x{:02x}, {} bytes — surfaced, not dropped)",
                            code,
                            payload.len()
                        ));
                    }
                }
                // Dialog/door/ground/social/… — keep this CLI compiling as ServerEvent grows.
                other => log(&format!("  (event) {other:?}")),
            }
        }

        // First loop after world entry: fire the one-shots.
        if got_world && !acted {
            acted = true;
            if let Some(msg) = say.take() {
                log(&format!("→ /say {msg}"));
                if let Err(e) = sess.say(&msg) {
                    eprintln!("say send failed: {e}");
                    break;
                }
            }
            if args.walk != 0.0 {
                match position {
                    Some((x, y, z)) => {
                        if let Err(e) = walk_along_y(&mut sess, x, y, z, args.walk) {
                            eprintln!("walk failed: {e}");
                            break;
                        }
                    }
                    None => log("no spawn position decoded — cannot walk"),
                }
            }
            if args.stay > 0 {
                stay_until = Some(Instant::now() + Duration::from_secs(args.stay));
                log(&format!(
                    "staying in-world for {}s (keepalive ping every 4s)…",
                    args.stay
                ));
            } else if !start_quit(&mut sess, args.quit, &mut quit_deadline) {
                break;
            }
        }

        // In-world stay window elapsed → log out.
        if let Some(deadline) = stay_until {
            if Instant::now() >= deadline {
                stay_until = None;
                if !start_quit(&mut sess, args.quit, &mut quit_deadline) {
                    log("stay window over — dropping the socket (link-death logout; use --quit for a clean /quit)");
                    break;
                }
            }
        }
        // Graceful quit in progress: leave once the server confirms (Quit 0xA4 → logged_out), or
        // after a safety timeout (a quit-timer server can hold us ~60s, longer if recently fighting).
        if let Some(deadline) = quit_deadline {
            if sess.logged_out() {
                log("\x1b[32m✓ clean logout confirmed — server saved + removed us; closing socket (no link-death)\x1b[0m");
                break;
            }
            if Instant::now() >= deadline {
                log("quit not confirmed within the timeout — closing anyway (was the character moving / in combat?)");
                break;
            }
        }
        // Overview arrived with no character to play → nothing more will happen.
        if sess.stuck_at_char_select() {
            break 'session;
        }
        // Silent stall before world entry → the sequence failed; give up.
        if !got_world && last_progress.elapsed() >= STALL_LIMIT {
            log("no server progress — the login/entry sequence stalled");
            break;
        }
    }

    println!();
    log(&format!("final phase: {:?}", sess.phase()));
    if !raw_counts.is_empty() {
        let total: usize = raw_counts.values().sum();
        log(&format!(
            "undecoded s2c packets: {} across {} distinct codes (all surfaced)",
            total,
            raw_counts.len()
        ));
    }
    if entity_updates > 0 {
        log(&format!(
            "decoded {entity_updates} entity movement updates (0xa1)"
        ));
    }
    if removals > 0 {
        log(&format!("culled {removals} entities that left view (0xe1)"));
    }
    if got_world {
        log("\x1b[32mP1 COMPLETE: logged in, picked a character, and ENTERED THE WORLD on the real server — headless.\x1b[0m");
        std::process::exit(0);
    } else if got_overview && sess.stuck_at_char_select() {
        log("\x1b[32mlogged in and decoded the character list; no character to play — stopped at char select.\x1b[0m");
        std::process::exit(0);
    } else if got_overview {
        log("character selected but world entry did not complete — inspect the exchange above.");
        std::process::exit(1);
    } else if got_login {
        log("login granted, but the session-id handshake or overview did not complete — inspect the exchange above.");
        std::process::exit(1);
    } else {
        log("login not granted — the server rejected our sequence; inspect the exchange above.");
        std::process::exit(1);
    }
}

/// Begin a clean logout if `--quit` was requested. Returns `true` when the caller should keep
/// looping (a `/quit` was sent, or is already pending — wait for the server's confirmation) and
/// `false` when no quit was asked for (the caller should drop the socket = link-death logout).
/// Sends `/quit` exactly once and arms a safety timeout.
fn start_quit(
    sess: &mut LiveSession,
    want_quit: bool,
    quit_deadline: &mut Option<Instant>,
) -> bool {
    if !want_quit {
        return false;
    }
    if quit_deadline.is_none() {
        log("→ /quit (clean logout — waiting for the server to save + remove us)");
        if let Err(e) = sess.quit() {
            eprintln!("quit send failed: {e}");
            return false; // fall back to a socket drop
        }
        // Generous timeout: a quit-timer server holds ~60s, longer just after combat.
        *quit_deadline = Some(Instant::now() + Duration::from_secs(70));
    }
    true
}

/// Walk `dist` game units along +y (−dist walks −y) at a legal speed, sending a position update
/// every 200 ms like the real client. Position persists on logout, so a re-login shows the new
/// spawn — movement is self-verifiable. Blocks the poll loop briefly; acceptable for a one-shot.
fn walk_along_y(sess: &mut LiveSession, x: f32, y: f32, z: f32, dist: f32) -> std::io::Result<()> {
    const SPEED: f32 = 100.0; // units/sec — walking pace, well under run speed (~191)
    const TICK_MS: u64 = 200;
    let step = SPEED * (TICK_MS as f32 / 1000.0) * dist.signum();
    let steps = (dist.abs() / step.abs()).ceil() as u32;
    log(&format!(
        "→ walking {dist:.0} units along y at {SPEED:.0} u/s ({steps} updates)…"
    ));
    let mut cur_y = y;
    for i in 0..steps {
        cur_y += step;
        // Last update reports speed 0 — we've stopped where we are.
        let speed = if i + 1 == steps { 0.0 } else { SPEED };
        sess.position_update(x, cur_y, z, speed, 100)?;
        std::thread::sleep(Duration::from_millis(TICK_MS));
    }
    log(&format!(
        "→ walk done: ({x:.0}, {cur_y:.0}, {z:.0}) — re-login to verify persistence"
    ));
    Ok(())
}
