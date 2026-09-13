//! Throughput checkpoint for the world model.
//!
//! Replays a captured session into `WorldState` and measures the two CPU paths that decide
//! client framerate under load: **ingest** (applying the packet flood) and **interest queries**
//! (what's near me / how many things to render). Then it scales the population synthetically to
//! show how the *current, single-threaded, naive-scan* baseline behaves at zerg sizes — the
//! number the spatial-index + parallel slice has to beat.
//!
//! Run:  cargo run --release -p caer-world --example worldbench -- [trace.bin]
//! Defaults to the frozen golden login trace if no path is given.

use std::io::BufReader;
use std::time::Instant;

use caer_protocol::session::ServerEvent;
use caer_world::{replay::decode_trace, world_data::load_mobs_tsv, WorldState};

const GOLDEN: &[u8] = include_bytes!("../tests/fixtures/rustdaoc_login_20260714.bin");

fn main() {
    // Optional `--mobs PATH`: use a real DB mob dump as the base population (else the golden
    // trace, or a capture path given as the first positional arg).
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(i) = args.iter().position(|a| a == "--mobs") {
        let path = args.get(i + 1).expect("--mobs needs a path");
        let file = std::fs::File::open(path).unwrap_or_else(|e| {
            eprintln!("could not read {path}: {e}");
            std::process::exit(1);
        });
        let mobs = load_mobs_tsv(BufReader::new(file));
        println!("mobs: {path} — {} real spawns loaded\n", mobs.len());
        let events: Vec<ServerEvent> = mobs.into_iter().map(ServerEvent::NpcInView).collect();
        run(&events);
        return;
    }

    let events = match args.first() {
        Some(path) => {
            let bytes = std::fs::read(path).unwrap_or_else(|e| {
                eprintln!("could not read {path}: {e}");
                std::process::exit(1);
            });
            println!("trace: {path}");
            decode_trace(&bytes)
        }
        None => {
            println!("trace: (frozen golden login trace)");
            decode_trace(GOLDEN)
        }
    };
    run(&events);
}

fn run(events: &[ServerEvent]) {
    let creates = events
        .iter()
        .filter(|e| matches!(e, ServerEvent::NpcInView(_) | ServerEvent::ObjectInView(_)))
        .count();
    let updates = events
        .iter()
        .filter(|e| matches!(e, ServerEvent::EntityUpdated(_)))
        .count();
    println!(
        "decoded {} events ({creates} creates, {updates} updates)\n",
        events.len()
    );

    // --- INGEST: apply the whole session flood, many times for a stable number ------------
    let ingest_iters = 2000;
    let t = Instant::now();
    let mut final_pop = 0;
    let mut unresolved = 0;
    for _ in 0..ingest_iters {
        let mut w = WorldState::new();
        for ev in events {
            w.apply(ev);
        }
        final_pop = w.len();
        unresolved = w.unresolved();
    }
    let elapsed = t.elapsed();
    let total_events = ingest_iters as u128 * events.len() as u128;
    let per_event_ns = elapsed.as_nanos() as f64 / total_events as f64;
    println!("INGEST");
    println!("  final population: {final_pop} entities ({unresolved} unresolved)");
    println!(
        "  {:.1} M events/sec  ({:.1} ns/event)",
        1000.0 / per_event_ns,
        per_event_ns
    );
    println!(
        "  full session ingest: {:.1} µs\n",
        elapsed.as_nanos() as f64 / ingest_iters as f64 / 1000.0
    );

    // --- QUERY: one frame's worth of interest work at the real population -----------------
    let mut w = WorldState::new();
    for ev in events {
        w.apply(ev);
    }
    let center = w
        .positions()
        .first()
        .map_or([560000, 510000], |p| [p[0], p[1]]);
    bench_queries("real population", &w, center);

    // --- SCALE: naive O(N) scan vs the grid-indexed O(nearby) path at zerg sizes ----------
    println!("\nSCALE — naive O(N) scan vs spatial grid (nearest-8, µs), and the speedup:");
    println!(
        "  {:>8}  {:>12}  {:>12}  {:>9}",
        "entities", "naive µs", "grid µs", "speedup"
    );
    for &target in &[500usize, 2_000, 8_000, 20_000, 50_000] {
        let big = inflate(&w, target);
        let c = [center[0], center[1]];
        let iters = 2000;

        let t = Instant::now();
        for _ in 0..iters {
            std::hint::black_box(big.nearest(c, 8));
        }
        let naive_us = t.elapsed().as_nanos() as f64 / iters as f64 / 1000.0;

        let t = Instant::now();
        for _ in 0..iters {
            std::hint::black_box(big.nearest_indexed(c, 8));
        }
        let grid_us = t.elapsed().as_nanos() as f64 / iters as f64 / 1000.0;

        println!(
            "  {:>8}  {:>12.2}  {:>12.3}  {:>8.0}x",
            big.len(),
            naive_us,
            grid_us,
            naive_us / grid_us
        );
    }

    // --- PER-FRAME RENDER PREP: serial vs rayon across all cores --------------------------
    let cores = std::thread::available_parallelism().map_or(0, |n| n.get());
    println!("\nRENDER-PREP per frame — extrapolate + cull + LOD for the visible set");
    println!("(serial 1 core vs dedicated render pool on a {cores}-core box; big radius => most visible):");
    println!(
        "  {:>8}  {:>12}  {:>12}  {:>9}",
        "entities", "serial µs", "parallel µs", "speedup"
    );
    for &target in &[2_000usize, 8_000, 20_000, 50_000, 150_000] {
        let big = inflate(&w, target);
        let cam = [center[0], center[1]];
        let radius = 5_000_000; // cover the whole synthetic spread — worst-case frame
        let iters = 500;

        let t = Instant::now();
        for _ in 0..iters {
            std::hint::black_box(big.render_set(cam, radius, 100));
        }
        let serial_us = t.elapsed().as_nanos() as f64 / iters as f64 / 1000.0;

        let t = Instant::now();
        for _ in 0..iters {
            std::hint::black_box(big.render_set_par(cam, radius, 100));
        }
        let par_us = t.elapsed().as_nanos() as f64 / iters as f64 / 1000.0;

        println!(
            "  {:>8}  {:>12.1}  {:>12.1}  {:>8.1}x",
            big.len(),
            serial_us,
            par_us,
            serial_us / par_us
        );
    }
}

fn bench_queries(label: &str, w: &WorldState, c: [i32; 2]) {
    let iters = 20_000;
    let t = Instant::now();
    for _ in 0..iters {
        std::hint::black_box(w.nearest(c, 8));
    }
    let near = t.elapsed().as_nanos() as f64 / iters as f64;
    let t = Instant::now();
    for _ in 0..iters {
        std::hint::black_box(w.count_within(c, 6000));
    }
    let within = t.elapsed().as_nanos() as f64 / iters as f64;
    println!("QUERY ({label}, {} entities)", w.len());
    println!("  nearest-8:     {near:.0} ns");
    println!("  count_within:  {within:.0} ns");
}

/// Grow the world to ~`target` entities by cloning the real ones on a jittered grid — a stand-in
/// for a keep-fight population until we capture a real one.
fn inflate(base: &WorldState, target: usize) -> WorldState {
    let mut w = WorldState::new();
    let src: Vec<([i32; 3], u16, String)> = base
        .iter()
        .map(|e| (e.pos, e.heading, e.name.to_string()))
        .collect();
    if src.is_empty() {
        return w;
    }
    let mut next_id: u16 = 1;
    let mut i = 0;
    while w.len() < target {
        let (pos, heading, name) = &src[i % src.len()];
        let ring = (i / src.len()) as i32;
        w.apply(&ServerEvent::NpcInView(caer_protocol::entities::Npc {
            object_id: next_id,
            speed: 0,
            heading: *heading,
            x: (pos[0] + ring * 137) as u32,
            y: (pos[1] + ring * 89) as u32,
            z: pos[2] as u16,
            model: 1,
            size: 50,
            level: 50,
            flags: 0,
            name: name.clone(),
            guild: String::new(),
        }));
        next_id = next_id.wrapping_add(1).max(1);
        i += 1;
    }
    w
}
