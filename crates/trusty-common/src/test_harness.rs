//! Runtime detection of a `cargo test` harness process, so shared code can
//! refuse to mutate the operator's production state from a test run (#4255).
//!
//! Why: trusty-search already had a compile-time guard for this
//! (`persistence::default_data_dir`'s `#[cfg(test)]` arm, issue #4094), but
//! `cfg(test)` is set per *compilation unit*, not per *process*. A crate's
//! `tests/` integration tests and its `[[bin]]` unit tests link the library
//! built WITHOUT `cfg(test)`, so the guard silently did not apply to them —
//! its own doc comment said so. Worse, the pollution that reached the live
//! `indexes.toml` most recently did not come from trusty-search's tests at
//! all: trusty-code's and trusty-mpm's tests call
//! [`crate::search_index::ensure_project_indexed`], which POSTs to whatever
//! real trusty-search daemon is discoverable. That write happens in a
//! DIFFERENT process, so no compile-time guard anywhere in trusty-search can
//! ever prevent it. A runtime check is the only mechanism that covers both.
//!
//! What: [`running_under_test_harness`] answers "is this process a cargo test
//! binary?" from the running executable's own path, with two env-var
//! overrides. Cargo compiles every test target to
//! `target/<profile>/deps/<name>-<metadata-hash>`; a `cargo run` binary
//! (`target/<profile>/<name>`) and an installed one (`~/.cargo/bin/<name>`)
//! never sit in a `deps/` directory, so the check is true for every test
//! target — lib unit tests, bin unit tests, integration tests, benches — and
//! false for every way a user actually runs the software.
//!
//! Scope boundary: this reads THIS process's path. A test that spawns the
//! real binary (`env!("CARGO_BIN_EXE_…")` resolves to `target/<profile>/<name>`,
//! outside `deps/`) produces a child that is not detected. Such a test must
//! set [`FORCE_ENV`] or `TRUSTY_DATA_DIR` on the child explicitly.
//!
//! Test: `detect_*` and `is_cargo_test_binary_*` in this module's `tests`.

use std::path::Path;

/// Forces [`running_under_test_harness`] to report `true`.
///
/// Set this on a child process a test spawns, so the child inherits the
/// parent's test-isolation even though its own path is outside `deps/`.
pub const FORCE_ENV: &str = "TRUSTY_TEST_HARNESS";

/// Opt-in escape hatch: forces [`running_under_test_harness`] to report
/// `false`, letting a test deliberately exercise real production state.
///
/// Outranks everything else, including [`FORCE_ENV`] and `cfg!(test)`. This
/// exists so "a test needs the real registry" stays an explicit, greppable
/// decision rather than a hole every accident falls through.
pub const ALLOW_PRODUCTION_ENV: &str = "TRUSTY_ALLOW_PRODUCTION_STATE";

/// Is this process a `cargo test` binary?
///
/// Why: production-state mutations (registering an index in the operator's
/// `indexes.toml`, POSTing to the live daemon) must never happen from a test
/// run. Callers gate those mutations on this (#4255).
/// What: [`ALLOW_PRODUCTION_ENV`] wins first (→ `false`), then [`FORCE_ENV`]
/// (→ `true`), then this crate's own `cfg!(test)`, then the executable-path
/// check described in the module doc.
/// Test: `running_under_test_harness_is_true_in_this_test_binary` — this
/// module's tests run in exactly the harness the function must detect.
pub fn running_under_test_harness() -> bool {
    detect(
        std::env::var(ALLOW_PRODUCTION_ENV).ok().as_deref(),
        std::env::var(FORCE_ENV).ok().as_deref(),
        cfg!(test),
        std::env::current_exe().ok().as_deref(),
    )
}

/// Pure decision table behind [`running_under_test_harness`], with every
/// input injected so the precedence is testable without touching process
/// state (env vars are process-global and race concurrent tests).
///
/// Test: the `detect_*` tests in this module cover each precedence rung.
fn detect(
    allow_production: Option<&str>,
    force: Option<&str>,
    compiled_as_test: bool,
    current_exe: Option<&Path>,
) -> bool {
    if is_truthy(allow_production) {
        return false;
    }
    if is_truthy(force) || compiled_as_test {
        return true;
    }
    current_exe.is_some_and(is_cargo_test_binary)
}

/// Shared truthiness parse for both override vars: `1`/`true`/`yes`/`on`,
/// case-insensitive. Anything else (including unset) is false.
fn is_truthy(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Does `exe` look like a cargo-compiled test binary?
///
/// Why: this is the load-bearing signal — it must be true for every test
/// target and false for every real invocation. Cargo puts test binaries in
/// `deps/` and appends a metadata hash to the file stem; neither is true of
/// `cargo run`'s `target/<profile>/<name>` or an installed `~/.cargo/bin/<name>`.
/// Requiring BOTH keeps a user who happens to have a `deps` directory on their
/// PATH from being misdetected as a test.
/// What: parent directory is named `deps` AND the file stem ends in
/// `-<hex>` with at least 8 hex digits.
/// Test: `is_cargo_test_binary_*`.
fn is_cargo_test_binary(exe: &Path) -> bool {
    if exe.parent().and_then(Path::file_name) != Some(std::ffi::OsStr::new("deps")) {
        return false;
    }
    let Some(stem) = exe.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    stem.rsplit_once('-').is_some_and(|(name, hash)| {
        !name.is_empty() && hash.len() >= 8 && hash.chars().all(|c| c.is_ascii_hexdigit())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_exe() -> PathBuf {
        PathBuf::from("/repo/target/debug/deps/integration_tests-f54141ba75b48011")
    }

    /// The whole point of the module: the real running test process must be
    /// detected, through the real public entry point, with no injection.
    ///
    /// Takes `ENV_LOCK` because this is the only test here that reads the
    /// process-global override vars, and a sibling test sets
    /// [`ALLOW_PRODUCTION_ENV`] for its own duration
    /// (`search_index_tests`' wire-body test). Without the lock the two race
    /// and this one fails for a reason that has nothing to do with detection.
    #[test]
    fn running_under_test_harness_is_true_in_this_test_binary() {
        let _guard = crate::data_dir::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert!(
            running_under_test_harness(),
            "a cargo test process must be detected as a test harness"
        );
    }

    #[test]
    fn detect_allow_production_outranks_everything() {
        assert!(
            !detect(Some("1"), Some("1"), true, Some(&test_exe())),
            "TRUSTY_ALLOW_PRODUCTION_STATE must win over force, cfg(test) and the path check"
        );
    }

    #[test]
    fn detect_force_env_wins_over_path_check() {
        let installed = PathBuf::from("/Users/me/.cargo/bin/trusty-search");
        assert!(detect(None, Some("true"), false, Some(&installed)));
    }

    #[test]
    fn detect_falls_back_to_exe_path() {
        assert!(detect(None, None, false, Some(&test_exe())));
    }

    #[test]
    fn detect_is_false_for_a_real_invocation() {
        for exe in [
            "/Users/me/.cargo/bin/trusty-search",
            "/opt/homebrew/bin/trusty-search",
            "/repo/target/debug/trusty-search",
            "/repo/target/release/trusty-search",
        ] {
            assert!(
                !detect(None, None, false, Some(Path::new(exe))),
                "{exe} must not be mistaken for a test binary"
            );
        }
    }

    #[test]
    fn detect_is_false_when_exe_is_unresolvable() {
        assert!(!detect(None, None, false, None));
    }

    #[test]
    fn is_cargo_test_binary_accepts_every_target_kind() {
        for exe in [
            // lib + bin unit tests, integration tests, benches
            "/repo/target/debug/deps/trusty_search-88c47ed536c1e550",
            "/repo/target/debug/deps/integration_tests-f54141ba75b48011",
            "/repo/target/release/deps/registry_isolation-0802fde7a9957161",
        ] {
            assert!(
                is_cargo_test_binary(Path::new(exe)),
                "{exe} is a cargo test binary"
            );
        }
    }

    #[test]
    fn is_cargo_test_binary_requires_the_metadata_hash() {
        // A `deps/` directory alone is not enough — a plain name with no
        // `-<hex>` suffix is not something cargo's test harness produces.
        assert!(!is_cargo_test_binary(Path::new("/repo/deps/trusty-search")));
        assert!(!is_cargo_test_binary(Path::new(
            "/repo/target/debug/deps/trusty-search-notahash"
        )));
        // Too short to be cargo's 16-hex metadata hash.
        assert!(!is_cargo_test_binary(Path::new(
            "/repo/target/debug/deps/thing-abc"
        )));
    }

    #[test]
    fn is_truthy_accepts_the_documented_spellings_only() {
        for yes in ["1", "true", "TRUE", "Yes", " on "] {
            assert!(is_truthy(Some(yes)), "{yes:?} must be truthy");
        }
        for no in ["0", "false", "", "no", "off", "maybe"] {
            assert!(!is_truthy(Some(no)), "{no:?} must not be truthy");
        }
        assert!(!is_truthy(None));
    }
}
