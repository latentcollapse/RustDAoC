//! Embed git commit/dirty into the caer-client binary for live-evidence provenance (Codex B1).

use std::process::Command;

fn main() {
    let commit = git(&["rev-parse", "HEAD"])
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    // Empty porcelain is a *clean* tree. Filtering empty stdout as None made every
    // clean freeze stamp dirty=true (fail-closed the wrong way).
    let dirty = match git(&["status", "--porcelain"]) {
        Some(s) => !s.is_empty(),
        None => true,
    };
    println!("cargo:rustc-env=CAER_BUILD_COMMIT={commit}");
    println!(
        "cargo:rustc-env=CAER_BUILD_DIRTY={}",
        if dirty { "true" } else { "false" }
    );
    // Rebuild when HEAD moves.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()
        .map(|s| s.trim().to_string())
}
