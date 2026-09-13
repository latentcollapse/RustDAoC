//! `clipsweep` — parse EVERY `.kfa` in `anims/` and group the failures by reason.
//!
//! The Catacombs-era clips were found to fail `read_clip` ("no animation tracks") while resolving
//! fine from the tables. This measures how big that hole actually is before anyone fixes it: a
//! format the reader rejects is invisible to any coverage number that only checks the tables.

use std::collections::BTreeMap;

fn main() {
    let root = caer_render::terrain::client_root();
    let mut ok = 0usize;
    let mut by_err: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut versions: BTreeMap<String, (usize, usize)> = BTreeMap::new(); // version -> (ok, fail)

    let Ok(rd) = std::fs::read_dir(root.join("anims")) else {
        eprintln!("no anims/ dir");
        return;
    };
    let mut files: Vec<_> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("kfa")))
        .collect();
    files.sort();

    for p in &files {
        let Ok(bytes) = std::fs::read(p) else {
            continue;
        };
        // The NIF header line is ASCII up to the first newline, and carries the version string.
        let ver = bytes
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| String::from_utf8_lossy(&bytes[..i]).trim().to_string())
            .unwrap_or_else(|| "<no header>".into());
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        match caer_assets::nif::read_clip(&bytes) {
            Ok(_) => {
                ok += 1;
                versions.entry(ver).or_default().0 += 1;
            }
            Err(e) => {
                by_err.entry(e.to_string()).or_default().push(stem);
                versions.entry(ver).or_default().1 += 1;
            }
        }
    }

    // Block-type histogram for the FAILING 10.1 clips: what does that generation actually use?
    let mut gb_types: BTreeMap<String, usize> = BTreeMap::new();
    let mut gb_seen = 0usize;
    for p in &files {
        let Ok(bytes) = std::fs::read(p) else {
            continue;
        };
        if !bytes.starts_with(b"Gamebryo") {
            continue;
        }
        let Ok(h) = caer_assets::nif::read_header(&bytes) else {
            continue;
        };
        gb_seen += 1;
        for t in &h.block_types {
            *gb_types.entry(t.clone()).or_default() += 1;
        }
        if gb_seen == 1 {
            println!(
                "-- first Gamebryo clip: {} --",
                p.file_name().unwrap().to_string_lossy()
            );
            println!("   {} blocks, types: {:?}\n", h.num_blocks, h.block_types);
        }
    }
    println!("-- block types across {gb_seen} Gamebryo clips --");
    let mut v: Vec<_> = gb_types.into_iter().collect();
    v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (t, n) in v.iter().take(20) {
        println!("  {n:>5}  {t}");
    }
    println!();

    let total = files.len();
    println!(
        "parsed {ok}/{total} clips ({:.1}%)\n",
        ok as f64 * 100.0 / total as f64
    );
    println!("-- failures by reason --");
    for (err, names) in &by_err {
        println!("  {:>5}  {err}", names.len());
        println!(
            "         e.g. {}",
            names.iter().take(6).cloned().collect::<Vec<_>>().join(", ")
        );
    }
    println!("\n-- by NIF header/version --");
    for (v, (o, f)) in &versions {
        println!("  ok {o:>5}  fail {f:>5}   {v}");
    }
}
