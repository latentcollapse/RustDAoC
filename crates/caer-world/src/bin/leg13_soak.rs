//! Leg 13 soak — long-session world-state stability with **recorded** RSS.
//!
//! Synthetic death/revive + zone-churn cycles against [`WorldState`]. Measures process RSS
//! (Linux `/proc/self/status` VmRSS) after warmup and after N cycles; writes a JSON report.
//!
//! ## Arms
//! - **clean**: full delete + particle drain; RSS must stay within a **proportional** band of
//!   baseline (`max(10% baseline, 512 KiB)` — not a flat +8 MiB floor). `ok: true` required.
//! - **negative**: skips `ObjectRemoved` + particle drain so state accumulates. Under the **same**
//!   criteria, must report `ok: false` with RSS (or entity count) visibly rising. If that arm
//!   stays green or flat, the instrument can't see leaks and the clean result is unfalsifiable.
//!
//! Honest limit (recorded in the artifact): WorldState synthetic churn — **not** a 6-hour
//! live client run.

use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process;

use caer_protocol::entities::{Npc, Player, StaticObject};
use caer_protocol::session::ServerEvent;
use caer_world::{WorldState, MAX_PARTICLE_EFFECTS};

fn rss_kib() -> Option<u64> {
    let f = fs::File::open("/proc/self/status").ok()?;
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kib: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kib);
        }
    }
    None
}

fn npc(id: u16, x: i32, y: i32, name: &str) -> ServerEvent {
    ServerEvent::NpcInView(Npc {
        object_id: id,
        speed: 0,
        heading: 0,
        x: x as u32,
        y: y as u32,
        z: 2400,
        model: 1,
        size: 50,
        level: 50,
        flags: 0,
        name: name.into(),
        guild: String::new(),
    })
}

fn player(id: u16, x: i32, y: i32, name: &str) -> ServerEvent {
    ServerEvent::PlayerInView(Player {
        object_id: id,
        session_id: id,
        x: x as f32,
        y: y as f32,
        z: 2400.0,
        heading: 0,
        model_unverified: 0,
        level: 50,
        realm: 1,
        flags: 0,
        name: name.into(),
        guild: String::new(),
        last_name: String::new(),
        custom: caer_protocol::customization::Customization::default(),
        eye_size: 0,
        lip_size: 0,
    })
}

fn static_obj(id: u16, x: i32, y: i32) -> ServerEvent {
    ServerEvent::ObjectInView(StaticObject {
        object_id: id,
        emblem: 0,
        heading: 0,
        x: x as u32,
        y: y as u32,
        z: 2400,
        model: 1,
        name: format!("obj-{id}"),
    })
}

#[derive(Clone, Copy)]
struct ChurnOpts {
    /// When true: skip ObjectRemoved + particle drain so state accumulates (negative control).
    leak: bool,
}

/// One churn cycle: populate many entities of every kind with equipment + particles, death/revive
/// self, region handoff, then (clean arm only) cull everything.
fn churn_cycle(w: &mut WorldState, cycle: u32, opts: ChurnOpts) {
    const POP: u16 = 400;
    // Leak arm must not recycle ids — reuse would overwrite columns instead of growing the store.
    let base = if opts.leak {
        cycle.saturating_mul(u32::from(POP)).wrapping_add(1) as u16
    } else {
        ((cycle % 50) * u32::from(POP)) as u16
    };

    for i in 0..POP {
        let id = base.wrapping_add(i).max(1);
        match i % 3 {
            0 => w.apply(&npc(id, 5000 + i as i32, 5000, &format!("mob-{id}"))),
            1 => w.apply(&player(id, 6000 + i as i32, 6000, &format!("pc-{id}"))),
            _ => w.apply(&static_obj(id, 7000 + i as i32, 7000)),
        }
        w.apply(&ServerEvent::EquipmentUpdated(
            caer_protocol::equipment::EquipmentUpdate {
                object_id: id,
                items: vec![caer_protocol::equipment::VisibleItem {
                    slot: caer_protocol::equipment::slot::TORSO,
                    model: 100 + (i % 50),
                    extension: Some(1),
                    ..Default::default()
                }],
                ..Default::default()
            },
        ));
    }

    // Spell flood — clean arm exercises the bound; leak arm retains without drain.
    for s in 0..(MAX_PARTICLE_EFFECTS as u16 + 32) {
        w.apply(&ServerEvent::SpellEffect(
            caer_protocol::spells::SpellEffectAnimation {
                caster_id: base.max(1),
                target_id: base.wrapping_add(1).max(1),
                spell_id: s,
                bolt_time: 0,
                no_sound: false,
                success: 1,
            },
        ));
    }
    if !opts.leak {
        let _ = w.drain_particle_effects();
    }

    let victim = base.max(1);
    w.apply(&ServerEvent::PlayerDied(
        caer_protocol::death::PlayerDeath {
            object_id: victim,
            killer_id: 0,
        },
    ));
    w.apply(&ServerEvent::PlayerRevived(
        caer_protocol::death::PlayerRevive { object_id: victim },
    ));

    w.apply(&ServerEvent::MerchantWindow(
        caer_protocol::merchant::MerchantWindow {
            window_type: 0,
            page: 0,
            items: vec![],
        },
    ));
    if !opts.leak {
        w.apply(&ServerEvent::RegionHandoff {
            ip: "127.0.0.1".into(),
            port: 10300,
        });
        assert!(
            w.merchant().is_none(),
            "region handoff must clear merchant transient"
        );
    }

    if !opts.leak {
        for i in 0..POP {
            let id = base.wrapping_add(i).max(1);
            w.apply(&ServerEvent::ObjectRemoved { object_id: id });
            assert!(
                w.equipment_of(id).is_none(),
                "deleted id {id} must not retain equipment"
            );
        }
    }
}

struct ArmResult {
    name: &'static str,
    baseline_kib: u64,
    peak_kib: u64,
    final_kib: u64,
    limit_kib: u64,
    entities_after_warmup: usize,
    entities_final: usize,
    /// Same criteria as the clean arm: within proportional RSS band + entity bound.
    /// Negative control **must** have `ok: false` (RSS visibly rising / entities retained).
    ok: bool,
    rss_delta_kib: i64,
}

fn slack_kib(baseline: u64) -> u64 {
    // Proportional band — Claude: the old max(25%, +8 MiB) gave ~4× baseline headroom on a
    // 2.7 MiB process, so only a quadrupling leak failed. Floor is noise, not a second budget.
    let pct = ((baseline as f64) * 0.10).ceil() as u64;
    pct.max(512)
}

fn detect_kib(baseline: u64) -> u64 {
    // Negative control must move at least this much (or 5% of baseline) to count as "visible".
    let pct = ((baseline as f64) * 0.05).ceil() as u64;
    pct.max(256)
}

fn run_arm(name: &'static str, cycles: u32, warmup: u32, leak: bool) -> ArmResult {
    let opts = ChurnOpts { leak };
    let mut world = WorldState::new();

    for c in 0..warmup {
        churn_cycle(&mut world, c, opts);
    }
    if !leak {
        let _ = world.drain_particle_effects();
    }
    let baseline_kib = rss_kib().unwrap_or(0);
    let entities_after_warmup = world.len();

    let mut peak_kib = baseline_kib;
    for c in 0..cycles {
        churn_cycle(&mut world, warmup + c, opts);
        if let Some(r) = rss_kib() {
            peak_kib = peak_kib.max(r);
        }
    }
    if !leak {
        let _ = world.drain_particle_effects();
    }
    let final_kib = rss_kib().unwrap_or(peak_kib);
    let entities_final = world.len();
    let limit_kib = baseline_kib.saturating_add(slack_kib(baseline_kib));
    let rss_delta_kib = final_kib as i64 - baseline_kib as i64;

    // Identical pass/fail rule for both arms — the negative control is only useful if it goes red
    // under the *same* criteria the clean arm uses.
    let ok = final_kib <= limit_kib && entities_final <= entities_after_warmup + 8;

    ArmResult {
        name,
        baseline_kib,
        peak_kib,
        final_kib,
        limit_kib,
        entities_after_warmup,
        entities_final,
        ok,
        rss_delta_kib,
    }
}

fn arm_json(a: &ArmResult) -> String {
    format!(
        "{{\n    \"arm\": \"{}\",\n    \"baseline_rss_kib\": {},\n    \"peak_rss_kib\": {},\n    \"final_rss_kib\": {},\n    \"limit_rss_kib\": {},\n    \"rss_delta_kib\": {},\n    \"entities_after_warmup\": {},\n    \"entities_final\": {},\n    \"ok\": {}\n  }}",
        a.name,
        a.baseline_kib,
        a.peak_kib,
        a.final_kib,
        a.limit_kib,
        a.rss_delta_kib,
        a.entities_after_warmup,
        a.entities_final,
        a.ok
    )
}

fn main() {
    let cycles: u32 = env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(80);
    let warmup: u32 = env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let out = env::var("CAER_SOAK_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/evidence/leg13_soak.json")
        });

    // Negative cycles can be fewer — we only need a visible rise, not a full soak.
    let neg_cycles = cycles.clamp(15, 40);

    let clean = run_arm("clean", cycles, warmup, false);
    let negative = run_arm("negative", neg_cycles, warmup.min(5), true);

    // Gate: clean stays green; negative must go red *and* move RSS (or entity count) enough that
    // the instrument is not blind. Flat negative.ok==true ⇒ unfalsifiable clean result.
    let neg_rss_visible = negative.final_kib
        >= negative
            .baseline_kib
            .saturating_add(detect_kib(negative.baseline_kib));
    let neg_entities_visible = negative.entities_final > negative.entities_after_warmup + 100;
    let negative_control_passed = !negative.ok && (neg_rss_visible || neg_entities_visible);
    let ok = clean.ok && negative_control_passed;
    let report = format!(
        "{{\n  \"leg\": 13,\n  \"instrument\": \"WorldState synthetic death/zone churn\",\n  \"honest_limit\": \"synthetic WorldState churn — not a 6h live client soak\",\n  \"slack\": \"max(10% baseline, 512 KiB)\",\n  \"warmup_cycles\": {warmup},\n  \"measure_cycles\": {cycles},\n  \"negative_cycles\": {neg_cycles},\n  \"particle_cap\": {MAX_PARTICLE_EFFECTS},\n  \"clean\": {},\n  \"negative\": {},\n  \"negative_control_passed\": {negative_control_passed},\n  \"ok\": {ok}\n}}\n",
        arm_json(&clean),
        arm_json(&negative),
    );

    if let Some(parent) = out.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&out, &report).unwrap_or_else(|e| {
        eprintln!("leg13_soak: failed to write {}: {e}", out.display());
        process::exit(2);
    });
    print!("{report}");
    eprintln!(
        "leg13_soak: clean ok={} Δ={} KiB | negative ok={} (want false) Δ={} KiB entities {}→{} | neg_control={} | wrote {}",
        clean.ok,
        clean.rss_delta_kib,
        negative.ok,
        negative.rss_delta_kib,
        negative.entities_after_warmup,
        negative.entities_final,
        negative_control_passed,
        out.display()
    );
    if !ok {
        if !clean.ok {
            eprintln!(
                "leg13_soak: CLEAN ARM FAILED — RSS or entity count outside proportional band"
            );
        }
        if !negative_control_passed {
            eprintln!(
                "leg13_soak: NEGATIVE CONTROL FAILED — leak arm stayed green or RSS/entities flat; instrument is blind"
            );
        }
        process::exit(1);
    }
}
