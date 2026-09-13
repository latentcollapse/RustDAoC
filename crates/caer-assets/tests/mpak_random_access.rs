//! Does fetching one MPAK member by directory offset return exactly what the sequential walk
//! returns?
//!
//! `caer_assets::read_named` trusts the directory's `offset`/`compressed_size` ints to jump
//! straight to a member's zlib stream. `caer_assets::read` ignores those ints and walks every
//! stream in order. The two are independent decodes of the same archive, so agreement between them
//! is real evidence rather than a round-trip: if the offsets were misread by even one field, the
//! seek would land mid-stream and inflate would fail or return different bytes.
//!
//! **Fails** when the retail tree is absent (REQ-025) — a check that did not run must not report
//! `pass`. Set `CAER_CLIENT` to point at the client tree.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    let r = caer_assets::client_dep::required_caer_client_root("mpak_random_access");
    assert!(
        r.join("pregame").is_dir(),
        "CAER_CLIENT has no pregame directory: {} — MPAK random access can only be checked against \
         real archives. REQ-025: a test that cannot run must not report pass.",
        r.display()
    );
    r
}

/// A spread of real archives: UI art, the big pre-world plate archive, a scene container, and a
/// zone data archive — different member counts, sizes and compression ratios.
fn sample(root: &Path) -> Vec<PathBuf> {
    let candidates = [
        "pregame/pregame.mpk",
        "pregame/charScreenAlb.npk",
        "pregame/realmdesc.mpk",
        "charman/summary.mpk",
        "data/login.mpk",
        "zones/zone471/dat471.mpk",
    ];
    let found: Vec<PathBuf> = candidates
        .iter()
        .map(|rel| root.join(rel))
        .filter(|p| p.is_file())
        .collect();
    assert!(
        found.len() >= 2,
        "expected several sample archives under {}, found {}",
        root.display(),
        found.len()
    );
    found
}

/// Every member, fetched by offset, is byte-identical to the same member from the full walk.
#[test]
fn random_access_matches_sequential_walk() {
    let root = root();
    let mut checked = 0usize;
    for path in sample(&root) {
        let bytes = std::fs::read(&path).unwrap();
        let walked =
            caer_assets::read(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(!walked.is_empty(), "{} is empty", path.display());

        let names: Vec<&str> = walked.iter().map(|e| e.name.as_str()).collect();
        let seeked = caer_assets::read_named(&bytes, &names)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));

        assert_eq!(
            seeked.len(),
            walked.len(),
            "{}: member count",
            path.display()
        );
        for (a, b) in seeked.iter().zip(&walked) {
            assert_eq!(a.name, b.name, "{}: member order", path.display());
            assert_eq!(
                a.data,
                b.data,
                "{}: {} differs between offset fetch and sequential walk ({} vs {} bytes)",
                path.display(),
                a.name,
                a.data.len(),
                b.data.len()
            );
            checked += 1;
        }
    }
    assert!(checked >= 40, "only compared {checked} members");
}

/// The directory's decompressed-size field agrees with what actually inflates. `read_named` relies
/// on this as its own consistency check, so it has to be true of real archives.
#[test]
fn directory_sizes_match_inflated_lengths() {
    let root = root();
    for path in sample(&root) {
        let bytes = std::fs::read(&path).unwrap();
        let (members, _) = caer_assets::directory(&bytes).unwrap();
        let walked = caer_assets::read(&bytes).unwrap();
        for (m, e) in members.iter().zip(&walked) {
            assert_eq!(
                m.size as usize,
                e.data.len(),
                "{}: {} directory size",
                path.display(),
                m.name
            );
        }
    }
}

/// Asking for a member the archive does not carry yields no entry — not a panic, not a neighbour.
#[test]
fn missing_member_is_absent_not_substituted() {
    let root = root();
    let bytes = std::fs::read(root.join("pregame/pregame.mpk")).unwrap();
    let got = caer_assets::read_named(&bytes, &["definitely_not_here.tga"]).unwrap();
    assert!(got.is_empty(), "got {} entries", got.len());
}

/// The red proof: this test exists to show the checks above can fail.
///
/// Truncating a real archive leaves the directory intact and its offsets pointing past the end of
/// the data. A reader that trusted those offsets blindly would panic or hand back short bytes; the
/// bounds and size guards must turn it into an error instead.
#[test]
fn truncated_archive_is_rejected_rather_than_silently_short() {
    let root = root();
    let bytes = std::fs::read(root.join("pregame/pregame.mpk")).unwrap();
    let (members, data_start) = caer_assets::directory(&bytes).unwrap();
    let last = members.last().expect("pregame.mpk has members").clone();

    // Cut halfway into the last member's stream, so the directory still claims bytes that are gone.
    let cut = data_start + last.offset as usize + last.compressed_size as usize / 2;
    let truncated = &bytes[..cut];
    let err = caer_assets::read_named(truncated, &[last.name.as_str()])
        .expect_err("truncated archive must not report success");
    let msg = err.to_string();
    assert!(
        msg.contains(&last.name) || msg.contains("past the"),
        "unhelpful error: {msg}"
    );
}
