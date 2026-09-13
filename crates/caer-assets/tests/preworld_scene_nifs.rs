//! Can we load the retail pre-world character-screen scenes? (H10 / B1)
//!
//! The captured creation forms show a rendered 3D environment behind the character, not a 2D
//! plate. Those scenes ship as `pregame/charScreen{Alb,Mid,Hib}.npk` — MPAK containers holding one
//! NetImmerse `.nif` each. All three parse; the two that did not were bounds tuned on smaller
//! files rejecting valid data, not missing format support.
//!
//! **Fails** when the retail tree is absent (REQ-025) — a check that did not run must not report
//! `pass`. Set `CAER_CLIENT` to point at the client tree.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    let root = caer_assets::client_dep::required_caer_client_root("preworld_scene_nifs");
    assert!(
        root.join("pregame").is_dir(),
        "CAER_CLIENT has no pregame directory: {} — these pre-world scene NIFs require real assets. \
         REQ-025: a test that cannot run must not report pass.",
        root.display()
    );
    root
}

fn scene(root: &Path, realm: &str) -> Option<Vec<u8>> {
    let p = root.join("pregame").join(format!("charScreen{realm}.npk"));
    let entries = caer_assets::open(&p).ok()?;
    entries.into_iter().next().map(|m| m.data)
}

/// All three scene archives exist and hold exactly one `.nif`.
#[test]
fn character_screen_scenes_are_present_and_single_member() {
    let root = root();
    for realm in ["Alb", "Mid", "Hib"] {
        let p = root.join("pregame").join(format!("charScreen{realm}.npk"));
        let entries =
            caer_assets::open(&p).unwrap_or_else(|e| panic!("charScreen{realm}.npk: {e}"));
        assert_eq!(entries.len(), 1, "charScreen{realm}.npk member count");
        assert_eq!(
            entries[0].name.to_ascii_lowercase(),
            format!("charscreen{}.nif", realm.to_ascii_lowercase()),
            "unexpected member name"
        );
    }
}

/// All three realm scenes parse with real geometry — not stubs.
///
/// Both gaps that kept Midgard and Hibernia out of this list were bounds tuned on smaller files
/// rejecting valid data, not missing format support:
/// `hibernia_scene_carries_an_oversized_extra_data_string` documents the string cap, and Midgard's
/// snowfall tripped a 10,000-particle cap with 10,075 slots.
///
/// Midgard's threshold is lower because its scene is mostly billboards and one particle emitter;
/// Albion and Hibernia are full modelled environments.
#[test]
fn character_screen_scenes_parse() {
    let root = root();
    for (realm, min_parts, min_geom) in [
        ("Alb", 100usize, 10_000usize),
        ("Hib", 100, 10_000),
        ("Mid", 10, 500),
    ] {
        let bytes = scene(&root, realm).unwrap_or_else(|| panic!("charScreen{realm}.npk"));
        let model = caer_assets::nif::read_model(&bytes)
            .unwrap_or_else(|e| panic!("charScreen{realm}.nif must parse: {e}"));
        let verts: usize = model.parts.iter().map(|p| p.positions.len()).sum();
        let tris: usize = model.parts.iter().map(|p| p.indices.len() / 3).sum();
        assert!(
            model.parts.len() > min_parts,
            "charScreen{realm}: expected a full scene, got {} parts",
            model.parts.len()
        );
        assert!(
            verts > min_geom,
            "charScreen{realm}: expected real geometry, got {verts} verts"
        );
        assert!(
            tris > min_geom,
            "charScreen{realm}: expected real geometry, got {tris} tris"
        );
        assert!(
            model.parts.iter().any(|p| p.texture.is_some()),
            "charScreen{realm}: scene parts must carry textures"
        );
    }
}

/// The reason Hibernia used to fail, pinned so the bound cannot quietly tighten again.
///
/// It was never a desync. `charScreenHib.nif` carries a `NiStringExtraData` named
/// `UserPropBuffer` holding **16,568 bytes** of 3ds Max user properties, and the parser's
/// 4096-byte "absurd string length" tripwire — correct for names and identifiers — rejected it as
/// corruption. Blocks #3804 and #3805 both decode byte-exactly by hand; the stream was fine the
/// whole time. It sat recorded as a "block desync" parser gap for a file that had nothing wrong
/// with it.
#[test]
fn hibernia_scene_carries_an_oversized_extra_data_string() {
    let root = root();
    let bytes = scene(&root, "Hib").expect("charScreenHib.npk");
    // Located without re-implementing the parser: each payload follows its own u32 length. The
    // file carries many `UserPropBuffer` blocks and most are tiny — the first is 20 bytes — so
    // take the largest rather than the first.
    let needle = b"UserPropBuffer";
    let len = bytes
        .windows(needle.len())
        .enumerate()
        .filter(|(_, w)| *w == needle)
        .filter_map(|(at, _)| {
            let len_at = at + needle.len();
            bytes
                .get(len_at..len_at + 4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
        })
        .max()
        .expect("charScreenHib.nif must contain a UserPropBuffer extra-data block");
    assert!(
        len > 4096,
        "this test is vacuous unless a payload actually exceeds the old bound (largest is {len})"
    );
    assert!(
        len < 1 << 20,
        "payload {len} exceeds the parser's own limit — the fix would not cover it"
    );
    // And the file parses with that payload present.
    caer_assets::nif::read_model(&bytes).expect("scene must parse with an oversized extra string");
}

/// **The eight creature NIFs that `NiPixelData` used to block.**
///
/// Ledger B9. The reader refused `NiPixelData` outright because two schema-derived layouts had both
/// desynced, and a wrong walk corrupts every block after it — refusing was the right call over
/// shipping a third guess. The layout was then measured off these files instead, and checked with
/// arithmetic the files supply: each mipmap descriptor's offset is the running sum of
/// `width * height * bytes_per_pixel`, and the stored total matches that sum.
///
/// The independent confirmation is positional. `CSRblue.NIF`'s pixel data ends at 0x12d7 and the
/// next block header sits at 0x12db — exactly one four-byte zero separator later. A skip that were
/// even one byte wrong would not land there, and the file would not reach its footer at EOF.
///
/// These eight are the known-bad fixtures; each must now parse and produce geometry.
#[test]
fn creature_nifs_that_nipixeldata_used_to_block_now_parse() {
    let root = root();
    let dir = root.join("figures");
    assert!(dir.is_dir(), "no figures/ under {}", root.display());

    // Four palettised (these also carry `NiPalette`) and four with compressed or true-colour mips.
    let fixtures = [
        "CSRblue.NIF",
        "CSRgreen.NIF",
        "CSRred.NIF",
        "CSRyellow.NIF",
        "krumpi.nif",
        "winter_jackfrost.nif",
        "winterlord_a.nif",
        "winterlord_b.nif",
    ];

    let mut failed: Vec<String> = Vec::new();
    let mut parts = 0usize;
    for name in fixtures {
        let path = dir.join(name);
        let Ok(bytes) = std::fs::read(&path) else {
            failed.push(format!("{name}: not present in the client tree"));
            continue;
        };
        match caer_assets::nif::read_model(&bytes) {
            Ok(m) => {
                assert!(
                    !m.parts.is_empty(),
                    "{name} parsed but produced no mesh parts — a skip that swallows the geometry \
                     is not a fix"
                );
                parts += m.parts.len();
            }
            Err(e) => failed.push(format!("{name}: {e}")),
        }
    }
    println!(
        "{} fixtures parsed, {parts} mesh parts total",
        fixtures.len() - failed.len()
    );
    assert!(
        failed.is_empty(),
        "{} of {} NiPixelData fixtures still fail:\n  {}",
        failed.len(),
        fixtures.len(),
        failed.join("\n  ")
    );
}

/// A floor on how much of the creature bank decodes, so a reader change cannot quietly lose models.
///
/// Not a target — the remaining failures are real and named in ledger B10 (three block-separator
/// desyncs, two `NiPosData` key types, one `NiFlipController`, two models with no visible
/// geometry). This only refuses a regression below what has been measured.
#[test]
fn the_figures_bank_keeps_its_decode_coverage() {
    let root = root();
    let dir = root.join("figures");
    assert!(dir.is_dir(), "no figures/ under {}", root.display());

    let mut total = 0usize;
    let mut ok = 0usize;
    for e in std::fs::read_dir(&dir).expect("figures dir").flatten() {
        let p = e.path();
        if !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("nif")) {
            continue;
        }
        total += 1;
        if std::fs::read(&p).is_ok_and(|b| caer_assets::nif::read_model(&b).is_ok()) {
            ok += 1;
        }
    }
    let pct = ok as f64 / total.max(1) as f64 * 100.0;
    println!("figures/: {ok} of {total} loose NIFs decode ({pct:.1}%)");
    assert!(
        total > 600,
        "only {total} NIFs found — the bank did not load"
    );
    // 617 before this work, then 625 (NiPixelData + NiPalette), 628 (NiTextKeyExtraData v10),
    // 630 (NiMaterialColorController target_color), 631 (NiFlipController).
    //
    // The two that remain are not decode failures: `CorpseLightNode.NIF` and `Invisible.NIF` walk
    // to completion and contain no drawable shape, which is what their names say they are. They
    // are counted here as "not ok" only because this helper asks for a model with geometry.
    assert!(
        ok >= 631,
        "decode coverage fell to {ok}/{total}; it was 631, with only the two deliberately empty \
         models outstanding"
    );
}

/// **The creature NIFs whose walk desynced mid-file.**
///
/// Ledger B10. The v10.1 branch read an `unknown_int` before the key count that only exists below
/// 10.0.1.0. That consumed the real count, so the first key's time (`0.0f`) became the count, zero
/// keys were parsed, and the walk stopped on the first key's string length — surfacing several
/// blocks later as "nonzero block separator", which pointed at the wrong block entirely.
///
/// Exactly three v10.1 files in `figures/` carry that block and all three failed: **the branch had
/// never parsed one successfully.**
///
/// The gargoyles and `ghostKing01` are the same shape of bug found the same way — a missing
/// `target_color` u16 on `NiMaterialColorController`, and an unimplemented `NiFlipController`. In
/// both cases the block that *reported* the failure was not the block at fault, which is why the
/// separator error now prints its offset and the preceding block's type.
#[test]
fn creature_nifs_that_desynced_mid_file_walk_to_the_end() {
    let root = root();
    let dir = root.join("figures");
    assert!(dir.is_dir(), "no figures/ under {}", root.display());

    let mut failed: Vec<String> = Vec::new();
    for name in [
        "7legspider.nif",
        "cata_spider.nif",
        "tree_npc.nif",
        // Two-byte desync from `NiMaterialColorController`'s missing `target_color`, which
        // surfaced one block later as an impossible `NiPosData` key type.
        "gargoyle_air.NIF",
        "gargoyle_ground.NIF",
        // Animated texture cycle — `NiFlipController`.
        "ghostKing01.nif",
    ] {
        let path = dir.join(name);
        let Ok(bytes) = std::fs::read(&path) else {
            failed.push(format!("{name}: not present"));
            continue;
        };
        match caer_assets::nif::read_model(&bytes) {
            Ok(m) => assert!(
                !m.parts.is_empty(),
                "{name} parsed but produced no mesh parts"
            ),
            Err(e) => failed.push(format!("{name}: {e}")),
        }
    }
    assert!(
        failed.is_empty(),
        "{} text-key fixture(s) still fail:\n  {}",
        failed.len(),
        failed.join("\n  ")
    );
}
