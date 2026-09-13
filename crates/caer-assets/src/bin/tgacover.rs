//! `tgacover` — decode every `.tga` the client ships and report what worked.
//!
//! Hand-written unit tests prove the decoder handles the variants I thought to write down. This runs
//! it over all 533 real files, grouped by header variant, so a format the client uses and I missed
//! shows up as a failure count rather than as a broken texture months later.
//!
//! Also sanity-checks the decoded result, because "did not error" is not "is correct": every image
//! must have exactly `w * h * 4` bytes, and a fully-transparent decode is reported as suspicious
//! (usually the sign of a misread alpha channel).
//!
//!   tgacover [--verbose]

use std::collections::BTreeMap;

fn main() {
    let verbose = std::env::args().any(|a| a == "--verbose");
    // `--raw <in.tga> <out.rgba>`: dump one decoded image as bare RGBA8 so it can be eyeballed.
    // Shape-correct bytes are not proof of a correct IMAGE — orientation and channel order both
    // produce plausible-looking garbage — and this is the cheapest way to actually look.
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--raw") {
        let (Some(inp), Some(outp)) = (args.get(i + 1), args.get(i + 2)) else {
            eprintln!("usage: tgacover --raw <in.tga> <out.rgba>");
            std::process::exit(2);
        };
        let bytes = std::fs::read(inp).expect("read tga");
        let img = caer_assets::tga::decode(&bytes).expect("decode tga");
        std::fs::write(outp, &img.rgba).expect("write raw");
        println!("{} {}x{}", outp, img.width, img.height);
        return;
    }
    let root = std::env::var("CAER_CLIENT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".wine/drive_c/Program Files (x86)/RustDAoC")
        });

    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();
    if files.is_empty() {
        eprintln!("tgacover: no .tga found under {}", root.display());
        std::process::exit(1);
    }

    // Keyed by the header fields that actually change the decode path.
    let mut stats: BTreeMap<(u8, u8, u8), (usize, usize)> = BTreeMap::new();
    let mut failures: Vec<String> = Vec::new();
    let mut blank: Vec<String> = Vec::new();
    let mut total_px = 0u64;

    for f in &files {
        let Ok(bytes) = std::fs::read(f) else {
            continue;
        };
        if bytes.len() < 18 {
            failures.push(format!("{}: shorter than a header", name(f)));
            continue;
        }
        let key = (bytes[2], bytes[16], bytes[1]);
        let e = stats.entry(key).or_insert((0, 0));
        match caer_assets::tga::decode(&bytes) {
            Ok(img) => {
                // "Did not error" is not "is correct" — check the shape of what came back.
                let want = (img.width as usize) * (img.height as usize) * 4;
                if img.rgba.len() != want {
                    failures.push(format!(
                        "{}: {} bytes for {}x{}",
                        name(f),
                        img.rgba.len(),
                        img.width,
                        img.height
                    ));
                    e.1 += 1;
                    continue;
                }
                // A wholly transparent image is legal but nearly always a misread alpha channel.
                if img.rgba.chunks(4).all(|p| p[3] == 0) {
                    blank.push(name(f));
                }
                total_px += u64::from(img.width) * u64::from(img.height);
                e.0 += 1;
                if verbose {
                    println!("  ok  {:<40} {}x{}", name(f), img.width, img.height);
                }
            }
            Err(err) => {
                failures.push(format!("{}: {err}", name(f)));
                e.1 += 1;
            }
        }
    }

    let ok: usize = stats.values().map(|(o, _)| o).sum();
    let bad: usize = stats.values().map(|(_, b)| b).sum();
    println!("=== {} .tga under {} ===", files.len(), root.display());
    println!(
        "  decoded {ok}, failed {bad}  ({:.1}% ok, {total_px} pixels)",
        100.0 * ok as f64 / (ok + bad).max(1) as f64
    );
    println!("\n  by variant (image_type, bpp, colour_map):");
    for ((it, bpp, cmt), (o, b)) in &stats {
        let kind = match it {
            1 => "colormapped",
            2 => "truecolor",
            3 => "greyscale",
            9 => "RLE colormapped",
            10 => "RLE truecolor",
            11 => "RLE greyscale",
            _ => "unknown",
        };
        println!("    type {it:>2} {bpp:>2}bpp cmap {cmt}  {kind:<16} ok {o:>4}  failed {b}");
    }

    if !blank.is_empty() {
        println!(
            "\n  fully transparent ({} — suspicious, check the alpha path):",
            blank.len()
        );
        for b in blank.iter().take(8) {
            println!("    {b}");
        }
    }
    if failures.is_empty() {
        println!("\n  no failures");
    } else {
        println!("\n  FAILURES ({}):", failures.len());
        for f in failures.iter().take(25) {
            println!("    {f}");
        }
    }
}

fn name(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("tga")) {
            out.push(p);
        }
    }
}
