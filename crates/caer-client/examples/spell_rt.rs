//! SCN-07 live falsifier — spell cast → cast-bar + particle presence (PlayScenario).
//!
//! Happy path: GM `&cast cast` / `&cast effect` inject SpellCastAnimation 0x72 then
//! SpellEffectAnimation 0x1B (empty SoloDAoC Spell table still emits these packets). WorldState
//! must show cast-bar fields while casting, then complete + nonempty particle drain after effect.
//!
//! ```bash
//! CAER_ACCOUNT=caer14d CAER_PASSWORD=caer14d CAER_SERVER=127.0.0.1:10311 \
//!   cargo \
//!   run -p caer-client --example spell_rt
//! ```
//!
//! **Refuse falsifier:** `CAER_SPELL_REFUSE=1` — clear target, fire UseSkill; must see **no**
//! SpellEffect and cast-bar must never advance. Named fake: Pass on UseSkill alone without 0x72/0x1B.

use std::time::{Duration, Instant};

use caer_client::live_harness::{self, LiveCredentials, LiveExit};
use caer_client::Config;
use caer_protocol::session::ServerEvent;
use caer_protocol::skills::{self, Skill, SkillKind};
use caer_world::WorldState;

const SPELL_ID: u16 = 407;

#[derive(Default)]
struct Track {
    x: f32,
    y: f32,
    z: f32,
    skills: Vec<Skill>,
    saw_cast: bool,
    saw_effect: bool,
    cast_bar_seen: bool,
    cast_bar_completed: bool,
    particles_drained: usize,
    chats: Vec<String>,
}

fn apply(t: &mut Track, world: &mut WorldState, e: &ServerEvent) {
    match e {
        ServerEvent::PlayerPosition { x, y, z, .. } => {
            t.x = *x;
            t.y = *y;
            t.z = *z;
        }
        ServerEvent::SkillsPage(page) => {
            skills::apply_page(&mut t.skills, page.clone());
        }
        ServerEvent::SpellCast(c) => {
            eprintln!(
                "spell_rt: SpellCast caster={} spell={} time={}",
                c.caster_id, c.spell_id, c.cast_time
            );
            t.saw_cast = true;
        }
        ServerEvent::SpellEffect(fx) => {
            eprintln!(
                "spell_rt: SpellEffect caster={} target={} spell={} success={}",
                fx.caster_id, fx.target_id, fx.spell_id, fx.success
            );
            t.saw_effect = true;
        }
        ServerEvent::ChatMessage { text, .. } => t.chats.push(text.clone()),
        _ => {}
    }
    world.apply(e);
    if world.cast_bar().is_some() {
        t.cast_bar_seen = true;
    }
    // After apply, a completed cast leaves cast_bar None while last_effect is Some.
    if t.saw_effect && world.cast_bar().is_none() && world.last_effect().is_some() {
        t.cast_bar_completed = true;
    }
    let drained = world.drain_particle_effects();
    if !drained.is_empty() {
        t.particles_drained += drained.len();
        eprintln!(
            "spell_rt: particle drain +{} (total {})",
            drained.len(),
            t.particles_drained
        );
    }
}

fn pick_spell_index(skills: &[Skill]) -> Option<(u8, u8, String)> {
    let usable: Vec<_> = skills
        .iter()
        .filter(|s| s.kind.is_usable())
        .cloned()
        .collect();
    if let Some((i, s)) = usable.iter().enumerate().find(|(_, s)| {
        matches!(
            s.kind,
            SkillKind::Spell | SkillKind::AbilitySpell | SkillKind::Song
        )
    }) {
        return Some((i as u8, skill_type_byte(s.kind), s.name.clone()));
    }
    usable
        .first()
        .map(|s| (0u8, skill_type_byte(s.kind), s.name.clone()))
}

fn skill_type_byte(k: SkillKind) -> u8 {
    match k {
        SkillKind::Specialization => 0,
        SkillKind::Ability => 1,
        SkillKind::Style => 2,
        SkillKind::Spell => 3,
        SkillKind::Song => 4,
        SkillKind::AbilitySpell => 5,
        SkillKind::RealmAbility => 6,
        SkillKind::Unknown(b) => b,
    }
}

fn main() {
    let creds = LiveCredentials::from_env();
    let server = creds.server.clone();
    let account = creds.account.clone();
    let password = creds.password.clone();
    let refuse = live_harness::env_flag("CAER_SPELL_REFUSE");
    let spell_id: u16 = std::env::var("CAER_SPELL_ID")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(SPELL_ID);

    let mut cfg = Config::new(&server, &account, &password);
    cfg.auto_select = true;

    eprintln!("spell_rt: server={server} account={account} refuse={refuse} spell_id={spell_id}");
    let mut sess = live_harness::connect(&cfg).unwrap_or_else(|e| live_harness::exit_connect(e));
    let mut track = Track::default();
    let mut world = WorldState::new();
    live_harness::wait_entered_world(&mut sess, live_harness::world_deadline(), |e| {
        apply(&mut track, &mut world, e)
    })
    .unwrap_or_else(|e| {
        LiveExit::from_wait_err(&e).bail(format!("FAIL world: {e}"));
    });
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(3), |e| {
        apply(&mut track, &mut world, e)
    });

    if refuse {
        // Falsifier: no target + UseSkill must not advance cast-bar or spawn effect particles.
        let _ = sess.target(0);
        live_harness::drain_or_exit(&mut sess, Duration::from_secs(1), |e| {
            apply(&mut track, &mut world, e)
        });
        track.saw_cast = false;
        track.saw_effect = false;
        track.cast_bar_seen = false;
        track.cast_bar_completed = false;
        track.particles_drained = 0;
        // Drop any presence queued during world entry.
        let _ = world.drain_particle_effects();

        let (idx, ty, name) = pick_spell_index(&track.skills).unwrap_or((0, 3, "forced".into()));
        eprintln!(
            "spell_rt: REFUSE UseSkill index={idx} type={ty} ({name}) at ({},{},{}) skills={}",
            track.x,
            track.y,
            track.z,
            track.skills.len()
        );
        sess.use_skill(idx, ty, track.x, track.y, track.z)
            .unwrap_or_else(|e| {
                eprintln!("FAIL use_skill: {e}");
                std::process::exit(7);
            });
        live_harness::drain_or_exit(&mut sess, Duration::from_secs(5), |e| {
            apply(&mut track, &mut world, e)
        });

        if track.saw_effect {
            eprintln!("FAIL REFUSE saw SpellEffect — refuse must not spawn effect");
            std::process::exit(1);
        }
        if track.cast_bar_seen || world.cast_bar().is_some() {
            eprintln!(
                "FAIL REFUSE cast-bar advanced (seen={} bar={:?})",
                track.cast_bar_seen,
                world.cast_bar()
            );
            std::process::exit(1);
        }
        if track.particles_drained > 0 || !world.particle_effects().is_empty() {
            eprintln!(
                "FAIL REFUSE particle spawn (drained={} queued={})",
                track.particles_drained,
                world.particle_effects().len()
            );
            std::process::exit(1);
        }
        println!(
            "PASS spell_rt REFUSE: UseSkill with no target — no SpellEffect, cast-bar never advanced, \
             particles empty. Observation that would fail if broken: invent cast-bar/particles on \
             UseSkill send alone."
        );
        return;
    }

    // PASS: inject 0x72 then 0x1B via GM (Spell table may be empty; these packets do not need it).
    eprintln!("spell_rt: PASS GM cast cast {spell_id}");
    let _ = sess.command(&format!("cast cast {spell_id}"));
    let deadline_cast = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline_cast && !track.saw_cast {
        live_harness::drain_or_exit(&mut sess, Duration::from_secs(1), |e| {
            apply(&mut track, &mut world, e)
        });
    }
    if !track.saw_cast {
        eprintln!("FAIL no SpellCast 0x72 after `cast cast {spell_id}` (need GM priv)");
        std::process::exit(1);
    }
    let bar = world.cast_bar();
    // May have already completed if effect raced; require we observed the bar at least once.
    if !track.cast_bar_seen && bar.is_none() {
        eprintln!("FAIL cast-bar never became active after SpellCast");
        std::process::exit(1);
    }
    if let Some(b) = world.cast_bar() {
        if b.spell_id != spell_id {
            eprintln!("FAIL cast-bar spell_id={} want {spell_id}", b.spell_id);
            std::process::exit(1);
        }
    }

    eprintln!("spell_rt: PASS GM cast effect {spell_id}");
    let _ = sess.command(&format!("cast effect {spell_id}"));
    let deadline_fx = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline_fx && !(track.saw_effect && track.particles_drained > 0) {
        live_harness::drain_or_exit(&mut sess, Duration::from_secs(1), |e| {
            apply(&mut track, &mut world, e)
        });
    }
    if !track.saw_effect {
        eprintln!("FAIL no SpellEffect 0x1B after `cast effect {spell_id}`");
        std::process::exit(1);
    }
    if track.particles_drained == 0 && world.particle_effects().is_empty() {
        eprintln!("FAIL no particle presence after successful SpellEffect");
        std::process::exit(1);
    }
    if world.cast_bar().is_some() {
        eprintln!(
            "FAIL cast-bar still active after effect (expected complete): {:?}",
            world.cast_bar()
        );
        std::process::exit(1);
    }
    if !track.cast_bar_completed && !track.cast_bar_seen {
        eprintln!("FAIL cast-bar never observed active→complete");
        std::process::exit(1);
    }

    println!(
        "PASS spell_rt: SpellCast→cast-bar active; SpellEffect→cast-bar complete + particle drain={}. \
         Observation that would fail if broken: Pass on chat alone, effect without particles, or \
         cast-bar without 0x72.",
        track.particles_drained.max(1)
    );
}
