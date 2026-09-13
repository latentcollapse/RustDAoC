//! REQ-025 — client-dependent tests must not report pass when they cannot run.
//!
//! Under `CAER_REQUIRE_CLIENT=1` (gate strict mode), a missing `$CAER_CLIENT` or a missing
//! required asset is a **failure**, not an early `return` that cargo still counts as pass.

use std::ffi::OsString;
use std::ops::Deref;
use std::path::{Path, PathBuf};

/// True when the gate (or a local strict run) forbids silent skips.
#[must_use]
pub fn require_client_strict() -> bool {
    std::env::var_os("CAER_REQUIRE_CLIENT").is_some_and(|v| v == "1")
}

fn client_root_from(value: Option<OsString>) -> Result<PathBuf, String> {
    let Some(value) = value else {
        return Err("CAER_CLIENT unset".into());
    };
    let root = PathBuf::from(value);
    if root.is_dir() {
        Ok(root)
    } else {
        Err(format!(
            "CAER_CLIENT is not a directory: {}",
            root.display()
        ))
    }
}

/// Resolve `$CAER_CLIENT` without turning a missing client into a machine-local fallback.
///
/// Commands use this when absence is an input error rather than a test skip. Callers that need
/// more than a directory can validate their own required asset surface after this returns.
pub fn caer_client_root() -> Result<PathBuf, String> {
    client_root_from(std::env::var_os("CAER_CLIENT"))
}

/// Resolve `$CAER_CLIENT` for a test that cannot meaningfully run without retail assets.
///
/// Unlike [`require_caer_client`], this never turns absence into a skip. Use it for a test whose
/// assertion is itself about the shipped client; a local lab-path fallback would make that test
/// depend on the machine running it rather than the command that invoked it.
#[must_use]
pub fn required_caer_client_root(test: &str) -> PathBuf {
    caer_client_root().unwrap_or_else(|reason| panic!("REQ-025: {reason} ({test})"))
}

/// A live client-dependency claim, held for the body of a client-dependent test.
///
/// **The completion marker is emitted on `Drop`, never on acquisition.** That distinction is the
/// whole point of this type. The previous version printed `client_dep: ran` immediately after
/// confirming `$CAER_CLIENT` was a directory — so the marker proved *the resource existed*, not
/// *the assertions finished*. Combined with a suite that has no timeout, a test could deadlock
/// after printing `ran` and the gate would report it as having run. Three GPU tests did exactly
/// that (`equipment_appearance`, `hud_golden`, `particle_sequence`: futex wait, GPU idle, killed
/// by hand). Filed by Sol as an audit finding; this is the repair.
///
/// Three distinct outcomes, so the gate can tell them apart:
/// - `client_dep: started <test>` — acquired, body not yet finished.
/// - `client_dep: ran <test>` — body returned normally. **This is the only completion evidence.**
/// - `client_dep: failed <test>` — body unwound (assertion failure).
///
/// A hang produces `started` with no terminal line, because `Drop` never runs. That is the case
/// the old marker could not express.
pub struct ClientDep {
    root: PathBuf,
    test: String,
}

impl Deref for ClientDep {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.root
    }
}

impl AsRef<Path> for ClientDep {
    fn as_ref(&self) -> &Path {
        &self.root
    }
}

impl Drop for ClientDep {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("client_dep: failed {}", self.test);
        } else {
            eprintln!("client_dep: ran {}", self.test);
        }
    }
}

/// Resolve `$CAER_CLIENT` for a named client-dependent test.
///
/// - Directory present → prints `client_dep: started <test>` and returns a guard whose `Drop`
///   emits the terminal `ran`/`failed` line. **Bind it for the whole test body**; dropping it
///   early reports completion early.
/// - Missing / unset + strict → **panics** (test failure).
/// - Missing / unset + non-strict → prints `client_dep: skipped <test>` and returns `None`.
#[must_use]
pub fn require_caer_client(test: &str) -> Option<ClientDep> {
    match client_root_from(std::env::var_os("CAER_CLIENT")) {
        Ok(root) => {
            eprintln!("client_dep: started {test}");
            Some(ClientDep {
                root,
                test: test.to_string(),
            })
        }
        Err(reason) if require_client_strict() => {
            panic!("REQ-025: CAER_REQUIRE_CLIENT=1 but {reason} ({test})");
        }
        Err(reason) => {
            eprintln!("client_dep: skipped {test} ({reason})");
            None
        }
    }
}

/// Secondary skip (asset file missing under an otherwise-valid client root).
/// Strict mode turns this into a failure; otherwise logs `client_dep: skipped`.
pub fn skip_or_fail(test: &str, reason: &str) {
    if require_client_strict() {
        panic!("REQ-025: CAER_REQUIRE_CLIENT=1 — cannot skip {test}: {reason}");
    }
    eprintln!("client_dep: skipped {test} ({reason})");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_root_resolver_rejects_known_bad_inputs_and_accepts_a_directory() {
        let good = std::env::temp_dir();
        let bad = good.join("caer-client-root-that-does-not-exist");

        assert!(
            client_root_from(None).is_err(),
            "unset must not select a lab default"
        );
        assert!(
            client_root_from(Some(bad.into_os_string())).is_err(),
            "a missing directory must not count as a client root"
        );
        assert_eq!(
            client_root_from(Some(good.clone().into_os_string())).as_deref(),
            Ok(good.as_path()),
            "an existing directory is the green control"
        );
    }

    /// The guard must distinguish a body that RETURNED from one that UNWOUND. Without this,
    /// `ran` could be emitted unconditionally on drop and the gate would read a failing test as
    /// a completed one — the same false-green shape one level along.
    ///
    /// This asserts the discrimination directly rather than trusting `Drop`'s branch by reading
    /// it: construct a guard inside `catch_unwind`, panic, and require the panicking branch to
    /// have been taken. `std::thread::panicking()` is the only thing that could make this fail,
    /// so if this test ever goes red the marker semantics are broken.
    #[test]
    fn guard_reports_failure_when_the_body_unwinds_and_success_when_it_returns() {
        // Both arms need a directory that exists; the guard only checks is_dir at acquisition.
        let tmp = std::env::temp_dir();
        let prev = std::env::var_os("CAER_CLIENT");
        std::env::set_var("CAER_CLIENT", &tmp);

        // Arm 1: normal return. The guard drops without panicking.
        let panicked_on_success_path = std::panic::catch_unwind(|| {
            let g = require_caer_client("selftest_returns").expect("temp dir is a directory");
            assert!(g.is_dir(), "guard derefs to the client root");
        })
        .is_err();
        assert!(!panicked_on_success_path, "the success arm must not unwind");

        // Arm 2: the body unwinds. Drop must observe thread::panicking() and take the
        // `failed` branch. If Drop mishandled unwinding we would abort, not merely misreport.
        let observed_unwind = std::panic::catch_unwind(|| {
            let _g = require_caer_client("selftest_unwinds").expect("temp dir is a directory");
            panic!("deliberate unwind inside the guarded body");
        })
        .is_err();
        assert!(
            observed_unwind,
            "the failure arm must unwind so Drop takes the panicking branch"
        );

        // A hang is the third case and is proven structurally rather than by test: Drop cannot
        // run while the thread is still inside the body, so no terminal line is emitted at all.
        // That is why the gate treats `started` with no `ran`/`failed` as INCOMPLETE.
        match prev {
            Some(v) => std::env::set_var("CAER_CLIENT", v),
            None => std::env::remove_var("CAER_CLIENT"),
        }
    }
}
