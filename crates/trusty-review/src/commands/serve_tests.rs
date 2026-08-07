//! Tests for the `serve` subcommand's dedup-store wiring (#5064).
//!
//! Why: the collision that #5064 records is decided entirely by what
//! `build_app_state` does with `{LOG_DIR}/dedup.redb`, so the decision is
//! tested at [`super::open_dedup_for`] — the one place the mode declaration
//! turns into a filesystem action.
//! What: asserts the stdio mode touches nothing, the HTTP mode opens the store,
//! a genuine open failure propagates instead of degrading to `None`, and two
//! posting-capable servers can share one `--log-dir`.
//! Test: this file.

use super::*;

/// A config whose `log_dir` points at a fresh temp dir.
fn config_with_log_dir(dir: &std::path::Path) -> ReviewConfig {
    let mut config = ReviewConfig::load(None);
    config.log_dir = dir.to_path_buf();
    config
}

/// Why: `serve --stdio` hardcodes `allow_posting: false` on every MCP tool, so
/// it has no reason to hold — or even create — the shared dedup file.
/// What: `NotNeeded` must return `None` and leave the filesystem untouched.
/// Test: this test. Fails before the fix, which created and locked the file.
#[test]
fn stdio_mode_never_creates_the_dedup_file() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_with_log_dir(dir.path());

    let dedup = open_dedup_for(&config, DedupNeed::NotNeeded).expect("must not fail");

    assert!(dedup.is_none(), "stdio mode must not hold a dedup store");
    assert!(
        !dir.path().join("dedup.redb").exists(),
        "stdio mode must not create dedup.redb (#5064)"
    );
}

/// Why: the HTTP mode serves the webhook handler, which posts.
/// What: `Required` opens the store and creates the file.
/// Test: this test.
#[test]
fn http_mode_opens_the_dedup_store() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_with_log_dir(dir.path());

    let dedup = open_dedup_for(&config, DedupNeed::Required).expect("must open");

    assert!(dedup.is_some(), "http mode must hold a dedup store");
    assert!(dir.path().join("dedup.redb").exists());
}

/// Why: the fail-open shape this issue exists to remove — an unopenable store
/// downgraded to a `warn!` and `dedup: None`, leaving a posting-capable server
/// running with no idempotency and a green health signal.
/// What: makes `dedup.redb` a directory so the open genuinely cannot succeed,
/// then asserts the error propagates.
/// Test: this test.
#[test]
fn http_mode_propagates_a_dedup_failure() {
    let dir = tempfile::tempdir().unwrap();
    // A directory where the redb file belongs: the open cannot succeed.
    std::fs::create_dir(dir.path().join("dedup.redb")).unwrap();
    let config = config_with_log_dir(dir.path());

    let err = open_dedup_for(&config, DedupNeed::Required)
        .expect_err("an unopenable required store must be an error, not `None`");
    assert!(
        format!("{err:#}").contains("required by a posting-capable server mode"),
        "error must name the requirement, got: {err:#}"
    );
}

/// Why: the #5064 topology under ADR-0034 — a console-spawned webhook worker
/// and the HTTP daemon are both posting-capable and both point at one
/// `--log-dir`. Neither may lock the other out.
/// What: two `Required` opens against the same log dir must both succeed.
/// Test: this test. Fails before the fix at the second open.
#[test]
fn two_posting_servers_can_share_one_log_dir() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_with_log_dir(dir.path());

    let first = open_dedup_for(&config, DedupNeed::Required).expect("first server must open");
    let second = open_dedup_for(&config, DedupNeed::Required)
        .expect("a second posting-capable server on the same --log-dir must open too (#5064)");

    assert!(first.is_some() && second.is_some());
}
