//! Tests for the `update` module.
//!
//! Why: Kept in a sibling file to respect the 500-line cap on `mod.rs`
//! while still using `#[cfg(test)]` so the test helpers are compiled only
//! in test mode.

use super::*;
use std::sync::Mutex;

/// Serialize tests that mutate environment variables to prevent races when
/// `cargo test` runs them on parallel threads.
static ENV_LOCK: Mutex<()> = Mutex::new(());

// ── semver helpers ──────────────────────────────────────────────────────

#[test]
fn semver_newer_returns_true() {
    assert!(is_newer("0.20.0", "0.19.0"));
    assert!(is_newer("1.0.0", "0.99.99"));
    assert!(is_newer("0.19.1", "0.19.0"));
}

#[test]
fn semver_equal_returns_false() {
    assert!(!is_newer("0.19.0", "0.19.0"));
}

#[test]
fn semver_older_returns_false() {
    assert!(!is_newer("0.18.0", "0.19.0"));
    assert!(!is_newer("0.19.0", "1.0.0"));
}

#[test]
fn semver_prerelease_stripped() {
    // Pre-release suffixes are stripped before comparison.
    assert!(!is_newer("0.19.0-beta.1", "0.19.0"));
    assert!(is_newer("0.20.0-alpha.1", "0.19.0"));
}

#[test]
fn semver_parse_strips_prerelease() {
    assert_eq!(parse_version("1.2.3-beta.1"), Some((1, 2, 3)));
    assert_eq!(parse_version("1.2.3+build.42"), Some((1, 2, 3)));
    assert_eq!(parse_version("1.2.3-rc.1+sha.abc"), Some((1, 2, 3)));
}

#[test]
fn semver_parse_handles_missing_patch() {
    assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
    assert_eq!(parse_version("1"), Some((1, 0, 0)));
}

#[test]
fn semver_parse_rejects_garbage() {
    assert_eq!(parse_version("not-a-version"), None);
    assert_eq!(parse_version(""), None);
}

// ── notice formatting ───────────────────────────────────────────────────

#[test]
fn notice_formats_correctly() {
    let info = UpdateInfo {
        crate_name: "trusty-search".to_owned(),
        current: "0.19.0".to_owned(),
        latest: "0.20.0".to_owned(),
    };
    let n = notice(&info);
    assert!(n.contains("trusty-search"), "crate name missing: {n}");
    assert!(n.contains("0.20.0"), "latest version missing: {n}");
    assert!(n.contains("0.19.0"), "current version missing: {n}");
    assert!(n.contains("cargo install"), "install command missing: {n}");
    assert!(n.contains("--locked"), "--locked flag missing: {n}");
}

// ── opt-out env var ────────────────────────────────────────────────────

#[tokio::test]
async fn check_throttled_skips_when_no_update_check_set() {
    // Set the env var while holding the lock, then drop the lock before
    // the await so clippy::await-holding-lock is satisfied.
    {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Safety: env mutation is serialized by ENV_LOCK; guard dropped
        // before the async call below.
        unsafe { std::env::set_var(NO_UPDATE_CHECK_ENV, "1") };
    }
    let result = check_throttled("trusty-search", "0.19.0").await;
    {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var(NO_UPDATE_CHECK_ENV) };
    }
    assert!(
        result.is_none(),
        "expected None when {NO_UPDATE_CHECK_ENV} is set"
    );
}

#[tokio::test]
async fn check_throttled_skips_when_ci_set() {
    {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var(CI_ENV, "true") };
    }
    let result = check_throttled("trusty-search", "0.19.0").await;
    {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var(CI_ENV) };
    }
    assert!(result.is_none(), "expected None when CI is set");
}

// ── cache freshness logic (uses a temp cache dir) ────────────────────────

/// Write a cache entry with the given `last_check_unix` timestamp and
/// `latest_version`, then call `check_throttled` and verify the result
/// matches `expected_is_some`. No real network is used because a fresh
/// cache entry suppresses the network call.
async fn run_cache_freshness_test(
    last_check_unix: u64,
    latest_version: &str,
    current_version: &str,
    expected_is_some: bool,
) {
    // Use a unique crate name to avoid cross-test cache pollution.
    let unique_crate = format!(
        "test-crate-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );

    let entry = CacheEntry {
        last_check_unix,
        latest_version: latest_version.to_owned(),
    };
    write_cache(&unique_crate, &entry);

    // The cache is fresh, so check_throttled returns the cached result
    // without any network call.
    let result = check_throttled(&unique_crate, current_version).await;
    assert_eq!(
        result.is_some(),
        expected_is_some,
        "freshness={expected_is_some}: latest={latest_version} current={current_version}"
    );

    // Clean up.
    let _ = std::fs::remove_file(cache_path(&unique_crate));
}

#[tokio::test]
async fn cache_fresh_returns_some_when_newer() {
    // Cache written 1 h ago (well within 24 h) with a newer version.
    run_cache_freshness_test(now_unix_secs() - 3600, "1.0.0", "0.19.0", true).await;
}

#[tokio::test]
async fn cache_fresh_returns_none_when_current() {
    // Cache written 1 h ago with the same version — already up to date.
    run_cache_freshness_test(now_unix_secs() - 3600, "0.19.0", "0.19.0", false).await;
}

// ── corrupt / missing cache file ──────────────────────────────────────

#[test]
fn corrupt_cache_returns_none() {
    // Write garbage bytes to the cache file; read_cache must return None.
    let unique_crate = format!("corrupt-test-{}", std::process::id());
    let path = cache_path(&unique_crate);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, b"this is not valid json {{{{");
    let result = read_cache(&unique_crate);
    let _ = std::fs::remove_file(&path);
    assert!(result.is_none(), "corrupt cache must yield None");
}

#[test]
fn missing_cache_returns_none() {
    let unique_crate = format!("missing-test-{}", std::process::id());
    // Ensure the file does not exist.
    let _ = std::fs::remove_file(cache_path(&unique_crate));
    let result = read_cache(&unique_crate);
    assert!(result.is_none(), "missing cache must yield None");
}

// ── cache round-trip ──────────────────────────────────────────────────

#[test]
fn cache_round_trip() {
    let unique_crate = format!("roundtrip-{}", std::process::id());
    let entry = CacheEntry {
        last_check_unix: 1_700_000_000,
        latest_version: "9.9.9".to_owned(),
    };
    write_cache(&unique_crate, &entry);
    let back = read_cache(&unique_crate);
    let _ = std::fs::remove_file(cache_path(&unique_crate));
    let back = back.expect("cache round-trip should succeed");
    assert_eq!(back.last_check_unix, 1_700_000_000);
    assert_eq!(back.latest_version, "9.9.9");
}

// ── upgrade primitive tests ────────────────────────────────────────────────

/// Verify the command constructed by `perform_upgrade` uses the right args.
///
/// Why: We can't actually run `cargo install` in a unit test without touching
/// the network and the filesystem. This test validates the command construction
/// logic by inspecting that `perform_upgrade` calls cargo with `install` and
/// `--locked` — validated by running it against a non-existent crate name and
/// checking that it fails with the right kind of error (cargo not found is
/// acceptable too, meaning the command was built).
/// Test: tagged #[ignore] because it shells out; run manually with
/// `cargo test -p trusty-common --features update-check -- --include-ignored`.
#[tokio::test]
#[ignore]
async fn perform_upgrade_fails_cleanly_on_nonexistent_crate() {
    let result = super::perform_upgrade("___trusty_test_crate_does_not_exist___").await;
    // Should be Err because cargo install would fail for a nonexistent crate.
    assert!(
        result.is_err(),
        "expected Err for nonexistent crate, got Ok"
    );
    let msg = result.unwrap_err().to_string();
    // The error message should mention cargo or the status.
    assert!(
        msg.contains("cargo install") || msg.contains("status") || msg.contains("exited"),
        "unexpected error message: {msg}"
    );
}

/// Verify that `verify_installed_binary` passes for `cargo`, which is always
/// on PATH for any developer machine.
///
/// Why: We need a real binary that is always present to test the happy path
/// without installing anything. `cargo --version` is a safe no-side-effect probe.
/// Test: tagged #[ignore] (shells out); run manually with `--include-ignored`.
#[tokio::test]
#[ignore]
async fn verify_installed_binary_passes_for_cargo() {
    let result = super::verify_installed_binary("cargo").await;
    assert!(
        result.is_ok(),
        "expected Ok for `cargo --version`, got: {:?}",
        result
    );
}

/// Verify that `verify_installed_binary` returns Err for a non-existent binary.
///
/// Why: Confirms the health gate catches missing binaries before a self-exit.
/// Test: sync test — no shell-out needed because `which` will quickly fail.
#[tokio::test]
async fn verify_installed_binary_fails_for_missing_binary() {
    let result = super::verify_installed_binary("___no_such_binary_xyz_999___").await;
    assert!(
        result.is_err(),
        "expected Err for non-existent binary, got Ok"
    );
}

/// Verify that `is_launchd_supervised` returns `false` in a normal test env.
///
/// Why: Unit tests run in a developer terminal / CI, neither of which is a
/// launchd-managed job. This catches regressions where the heuristic fires
/// too eagerly.
/// Test: the test sets `TERM_PROGRAM` to a non-empty value (mimicking an
/// interactive terminal session) and clears `XPC_SERVICE_NAME` to ensure the
/// fast path returns false.
#[test]
fn is_launchd_supervised_returns_false_in_test_env() {
    // Env mutations must be serialised with the same lock used by the other
    // env-mutating tests.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("TERM_PROGRAM", "TestRunner");
        std::env::remove_var("XPC_SERVICE_NAME");
    }
    let result = super::is_launchd_supervised();
    unsafe {
        std::env::remove_var("TERM_PROGRAM");
    }
    // In a terminal (TERM_PROGRAM set) the function must return false even
    // if XPC_SERVICE_NAME were somehow present.
    assert!(
        !result,
        "is_launchd_supervised returned true inside a test terminal env"
    );
}

// ── live crates.io integration test (requires network) ───────────────────────
// Tagged #[ignore] so it is skipped in normal CI runs.

#[tokio::test]
#[ignore]
async fn live_crates_io_with_old_version_returns_some() {
    // Deliberately old version — should show trusty-search is newer.
    let result = check_crates_io("trusty-search", "0.0.1").await;
    assert!(
        result.is_some(),
        "expected Some(UpdateInfo) for old version 0.0.1 — is network available?"
    );
    let info = result.unwrap();
    println!("crates.io returned: latest={}", info.latest);
    assert!(
        !info.latest.is_empty(),
        "latest version should not be empty"
    );
    // Verify the notice string renders correctly
    let n = notice(&info);
    println!("Notice: {n}");
    assert!(
        n.contains("cargo install trusty-search --locked"),
        "notice missing install cmd: {n}"
    );
    assert!(n.contains(&info.latest), "notice missing latest version");
    assert!(n.contains("0.0.1"), "notice missing current version");
}
