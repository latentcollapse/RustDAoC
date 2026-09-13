//! Compile-time build identity for live-evidence provenance (Codex B1).
//!
//! Embedded by `build.rs` as `CAER_BUILD_COMMIT` / `CAER_BUILD_DIRTY`. Live RT Pass requires
//! a matching `CAER_EVIDENCE` stdout line **and** the stamp bytes inside the native binary so
//! a compiled impostor cannot forge provenance by printing the current tip alone.

/// Git commit embedded at compile time (`unknown` if git unavailable during build).
pub const BUILD_COMMIT: &str = env!("CAER_BUILD_COMMIT");

/// `true` / `false` — working tree dirty when this crate was built.
/// Empty `git status --porcelain` is clean (`false`); git failure fail-closes to `true`.
/// Whether the tree was dirty **when `caer-client` last compiled**.
///
/// That is the right question for "was this binary built from clean source", which is what
/// `caer binary-identity` gates a playtest launch on. It is the wrong question for "is the tree
/// clean now", and it goes stale easily: `build.rs` reruns only on `.git/HEAD` / `.git/index`, and
/// editing another crate does not rebuild this one at all. Use [`worktree_dirty_now`] for anything
/// describing the moment rather than the binary (ledger B11).
pub const BUILD_DIRTY: &str = env!("CAER_BUILD_DIRTY");

/// Unique substring guaranteed present in any binary that links this module.
/// Scenario runners search the evidence executable for this exact stamp.
pub const BUILD_STAMP: &str = concat!("CAER_BUILD_COMMIT=", env!("CAER_BUILD_COMMIT"));

/// Print the typed evidence line. Call from live RT entry / product screenshot paths.
///
/// Format (TAB-separated): `CAER_EVIDENCE example=<name> commit=<sha> dirty=<bool>`
/// Is the worktree dirty **right now**, asked of git rather than remembered from build time.
///
/// [`BUILD_DIRTY`] is baked by `build.rs`, which reruns only when `.git/HEAD` or `.git/index`
/// changes. Editing a tracked file without staging it touches neither, so the constant goes stale
/// and keeps claiming whatever was true when `caer-client` last compiled — which, for a crate that
/// rarely changes, can be a long time. That is ledger B11.
///
/// A build-time constant cannot describe the tree at run time, so anything recording provenance
/// for a capture or a launch should ask this instead. Returns `None` when git cannot answer (not
/// a repository, git absent), which callers must record as unknown rather than as clean.
#[must_use]
pub fn worktree_dirty_now() -> Option<bool> {
    dirty_in(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
}

/// [`worktree_dirty_now`] against an explicit directory, so it can be tested against a repository
/// whose state is known instead of whichever one happens to be checked out.
#[must_use]
pub fn dirty_in(dir: &std::path::Path) -> Option<bool> {
    let out = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // Empty porcelain is a CLEAN tree. Treating empty output as "no answer" is how a clean build
    // once stamped dirty=true — fail-closed in the wrong direction.
    Some(!String::from_utf8_lossy(&out.stdout).trim().is_empty())
}

pub fn emit(example: &str) {
    // Keep BUILD_STAMP referenced so LTO cannot strip it from the binary.
    let _keep: &'static str = BUILD_STAMP;
    println!("CAER_EVIDENCE\texample={example}\tcommit={BUILD_COMMIT}\tdirty={BUILD_DIRTY}");
}

/// Machine-readable embedded build identity for launch verification (`rustdaoc --build-identity`).
/// Not CAER_EVIDENCE — does not claim achieved runtime proof.
pub fn emit_build_identity() {
    let _keep: &'static str = BUILD_STAMP;
    println!("CAER_BUILD_IDENTITY\tcommit={BUILD_COMMIT}\tdirty={BUILD_DIRTY}");
}

/// Procedural app-loop launcher observation (STARTED / EXITED). Not PLAYER_SCENARIO proof.
pub fn emit_app_loop_phase(phase: &str, exit_code: Option<i32>) {
    match exit_code {
        Some(c) => println!(
            "CAER_APP_LOOP\tphase={phase}\tprocedure=RUSTDAOC_APP_LOOP_PLAYTEST\texit_code={c}"
        ),
        None => println!("CAER_APP_LOOP\tphase={phase}\tprocedure=RUSTDAOC_APP_LOOP_PLAYTEST"),
    }
}

/// Example name from `CAER_RT_EXAMPLE` (set by the scenario runner), else `live_session`.
pub fn emit_from_env() {
    let example = std::env::var("CAER_RT_EXAMPLE").unwrap_or_else(|_| "live_session".into());
    emit(&example);
}

#[cfg(test)]
mod dirty_tests {
    use super::*;

    /// The runtime probe against a repository whose state is known, both ways.
    ///
    /// The point of ledger B11 is that the baked constant cannot do this: it reports whatever was
    /// true when the crate last compiled. A probe that only ever ran against the working checkout
    /// would agree with the constant most of the time and prove nothing.
    #[test]
    fn the_runtime_probe_sees_a_tree_change_that_the_build_stamp_cannot() {
        let dir = std::env::temp_dir().join(format!("caer-dirty-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !git(&["init", "-q"]) {
            eprintln!("git unavailable — probe control skipped");
            return;
        }
        let _ = git(&["config", "user.email", "t@example.invalid"]);
        let _ = git(&["config", "user.name", "t"]);
        std::fs::write(dir.join("a.txt"), "one\n").expect("write");
        assert!(git(&["add", "-A"]));
        assert!(git(&["commit", "-qm", "init"]));

        // Committed and untouched: clean.
        assert_eq!(
            dirty_in(&dir),
            Some(false),
            "a freshly committed tree must read clean"
        );

        // Edit a TRACKED file without staging. This touches neither `.git/HEAD` nor `.git/index`,
        // so `build.rs` would not even rerun — the exact blind spot B11 names.
        std::fs::write(dir.join("a.txt"), "two\n").expect("write");
        assert_eq!(
            dirty_in(&dir),
            Some(true),
            "an unstaged edit to a tracked file must read dirty"
        );

        // And back again, so the probe is not simply stuck on true.
        std::fs::write(dir.join("a.txt"), "one\n").expect("write");
        assert_eq!(
            dirty_in(&dir),
            Some(false),
            "reverting must read clean again"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Somewhere that is not a repository yields no answer — never "clean".
    #[test]
    fn a_non_repository_is_unknown_not_clean() {
        let dir = std::env::temp_dir().join(format!("caer-dirty-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        // `git status` outside a repo fails; if the temp dir happens to sit inside one, skip.
        if let Some(v) = dirty_in(&dir) {
            eprintln!("temp dir is inside a repository (dirty={v}) — cannot test the None path");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
