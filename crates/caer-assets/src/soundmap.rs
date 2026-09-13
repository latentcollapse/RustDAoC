//! Logical sound name → on-disk WAV path (leg 9 / presentation-audio).
//!
//! `sounds.dat` schedules **logical** names (`g_Thunder`, `s_Bed_…`). The client maps those to
//! files under `sounds/`. Full authored table is still open (RESUME: some names have no file
//! anywhere). This module only accepts mappings **proven by a file existing** under a
//! deterministic strip of the logical name — never invents stems.
//!
//! Resolution rule (OWN_CAPTURE / client-file evidence):
//! 1. Strip a leading `g_` / `s_` / `c_` prefix if present.
//! 2. Candidate stems: exact remainder, then `remainder1`…`remainder9`.
//! 3. Keep only stems whose `.wav` exists under `$CAER_CLIENT/sounds/` (case-sensitive first,
//!    then a case-insensitive directory walk of that one folder).
//! 4. If nothing matches → `None` (silent). That is how `g_PrairieWind` stays unmapped.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// Resolve a logical `sounds.dat` name to one or more candidate WAV paths under `client_root`.
///
/// Returns `None` when no client file matches — callers must not invent a file name.
#[must_use]
pub fn resolve_logical_wavs(client_root: &Path, logical: &str) -> Option<Vec<PathBuf>> {
    let sounds = client_root.join("sounds");
    if !sounds.is_dir() {
        return None;
    }
    let base = strip_logical_prefix(logical);
    if base.is_empty() {
        return None;
    }
    let mut stems = vec![base.to_string()];
    for i in 1..=9 {
        stems.push(format!("{base}{i}"));
    }
    // Client packs often use zero-padded / underscore variants (EvadeWhoosh_01, StoneImpact01).
    for i in 1..=9 {
        stems.push(format!("{base}_{i:02}"));
        stems.push(format!("{base}{i:02}"));
    }
    let listing = sounds_listing(&sounds);
    let listing_ref = listing.as_deref();
    let mut out = Vec::new();
    for stem in &stems {
        if let Some(p) = find_wav_in(&sounds, stem, listing_ref) {
            out.push(p);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn strip_logical_prefix(logical: &str) -> &str {
    let b = logical.as_bytes();
    if b.len() > 2 && (b[1] == b'_') && matches!(b[0], b'g' | b's' | b'c' | b'G' | b'S' | b'C') {
        &logical[2..]
    } else {
        logical
    }
}

fn find_wav_in(
    sounds_dir: &Path,
    stem: &str,
    listing: Option<&[(String, PathBuf)]>,
) -> Option<PathBuf> {
    let direct = sounds_dir.join(format!("{stem}.wav"));
    if direct.is_file() {
        return Some(direct);
    }
    // Authored stems are CamelCase; many WAVs are lowercase. Direct lowercase is still
    // file-existence proof (not an invented stem).
    let want = format!("{stem}.wav").to_ascii_lowercase();
    let lower_direct = sounds_dir.join(&want);
    if lower_direct != direct && lower_direct.is_file() {
        return Some(lower_direct);
    }
    if let Some(listing) = listing {
        if let Some((_, p)) = listing.iter().find(|(lower, _)| lower == &want) {
            return Some(p.clone());
        }
    }
    // Windows-authored trees disagree on case; walk this one directory only.
    let entries = std::fs::read_dir(sounds_dir).ok()?;
    for e in entries.flatten() {
        let name = e.file_name();
        let s = name.to_string_lossy();
        if s.to_ascii_lowercase() == want {
            let p = e.path();
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn sounds_listing(sounds_dir: &Path) -> Option<Vec<(String, PathBuf)>> {
    static LOCK: Mutex<()> = Mutex::new(());
    let _g = LOCK.lock().ok()?;
    for _ in 0..4 {
        let entries = match std::fs::read_dir(sounds_dir) {
            Ok(e) => e,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
        };
        let mut out = Vec::new();
        for e in entries.flatten() {
            let p = e.path();
            if p.is_file() {
                let lower = e.file_name().to_string_lossy().to_ascii_lowercase();
                out.push((lower, p));
            }
        }
        if !out.is_empty() {
            return Some(out);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

/// Pick one variant for playback (stable for a given salt).
#[must_use]
pub fn pick_variant(paths: &[PathBuf], salt: u64) -> Option<&Path> {
    if paths.is_empty() {
        return None;
    }
    let i = (salt as usize) % paths.len();
    Some(paths[i].as_path())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn unknown_logical_name_is_none_not_invented() {
        let root = Path::new("/nonexistent");
        assert!(resolve_logical_wavs(root, "g_PrairieWind").is_none());
        assert!(resolve_logical_wavs(root, "definitely_fake").is_none());
    }

    #[test]
    fn strip_prefix_examples() {
        assert_eq!(strip_logical_prefix("g_Thunder"), "Thunder");
        assert_eq!(strip_logical_prefix("s_Lavaflow"), "Lavaflow");
        assert_eq!(strip_logical_prefix("Thunder"), "Thunder");
    }

    #[test]
    fn thunder_and_exact_beds_resolve_when_client_present() {
        let Some(root) = std::env::var_os("CAER_CLIENT") else {
            eprintln!("skip: CAER_CLIENT unset");
            return;
        };
        // `client_dep` tests briefly point CAER_CLIENT at temp_dir(); wait for the real tree.
        let root = {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut p = PathBuf::from(&root);
            while !p.join("sounds").is_dir() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "CAER_CLIENT/sounds not a directory (client_dep env race?)"
                );
                std::thread::sleep(Duration::from_millis(20));
                p = PathBuf::from(std::env::var_os("CAER_CLIENT").unwrap_or_default());
            }
            p
        };
        let paths = resolve_logical_wavs(&root, "g_Thunder")
            .expect("g_Thunder → thunder1..6.wav under CAER_CLIENT/sounds");
        assert!(
            paths.len() >= 6,
            "expected six thunder variants, got {paths:?}"
        );
        // Case: logical keeps CamelCase after prefix; file is lower-case thunderN.wav —
        // find_wav must be case-insensitive on the sounds/ directory.
        let lava = resolve_logical_wavs(&root, "s_Lavaflow").expect("s_Lavaflow → lavaflow.wav");
        assert_eq!(lava.len(), 1, "{lava:?}");
        // Proven non-mapping: Prairie has no file under any strip of the name.
        assert!(
            resolve_logical_wavs(&root, "g_PrairieWind").is_none(),
            "Prairie must stay unmapped — inventing a stem would be the defect"
        );
        // Underscore-padded variants used by combat/evade beds.
        let evade =
            resolve_logical_wavs(&root, "EvadeWhoosh").expect("EvadeWhoosh → EvadeWhoosh_01..04");
        assert!(evade.len() >= 4, "{evade:?}");
        let shield = resolve_logical_wavs(&root, "Shield_ImpactLight")
            .expect("Shield_ImpactLight → Shield_ImpactLight01..04");
        assert!(shield.len() >= 4, "{shield:?}");
    }
}
