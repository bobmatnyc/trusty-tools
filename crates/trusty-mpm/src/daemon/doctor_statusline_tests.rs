//! Tests for the `tm doctor` `statusline` check (#7617).
//!
//! Why: this check is the standing answer to "is the 💸 segment wired and can
//! it render", which three separate investigations had to establish by hand.
//! Every verdict it can return is pinned here against injected readings, so it
//! never depends on the machine running the suite.
//! Test: this file IS the test module.

use super::*;

/// A settings file holding `entry` under `statusLine`, or no key at all.
fn settings(entry: Option<serde_json::Value>) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    let body = match entry {
        Some(value) => serde_json::json!({ "statusLine": value }),
        None => serde_json::json!({ "outputStyle": "trusty-mpm" }),
    };
    std::fs::write(&path, serde_json::to_string_pretty(&body).unwrap()).expect("seed");
    (dir, path)
}

/// Test: itself.
#[test]
fn a_tier_with_no_entry_is_reported_missing() {
    let (_dir, path) = settings(None);
    assert_eq!(tier_state(&path), TierState::Missing);

    let absent = std::path::Path::new("/definitely/not/here/settings.json");
    assert_eq!(tier_state(absent), TierState::Missing);
}

/// Why (#7262): a command left behind by a Cargo build tree that has since been
/// cleaned is the damage class this check exists to name.
/// Test: itself.
#[test]
fn a_tier_with_a_stale_command_is_reported_stale() {
    let (_dir, path) = settings(Some(serde_json::json!({
        "type": "command",
        "command": "/definitely/not/here/tm statusline",
        "padding": 0
    })));
    assert_eq!(tier_state(&path), TierState::Stale);
}

/// A command naming a binary that exists and is not a build artifact.
///
/// Why: the test binary's own path IS an ephemeral build path, so
/// `std::env::current_exe()` reads as stale (#2229) and cannot stand in for a
/// wired entry. `/bin/sh` exists on every host this runs on and is not one.
fn live_statusline_command() -> String {
    "/bin/sh statusline".to_string()
}

/// Test: itself.
#[test]
fn a_tier_with_a_live_command_is_reported_wired() {
    let (_dir, path) = settings(Some(serde_json::json!({
        "type": "command",
        "command": live_statusline_command(),
        "padding": 0
    })));
    assert_eq!(tier_state(&path), TierState::Wired);
}

/// One tier in the given state, for the fold's inputs.
fn tier(state: TierState) -> (PathBuf, TierState) {
    (PathBuf::from("/tmp/example/.claude/settings.json"), state)
}

/// Test: itself.
#[test]
fn a_wired_machine_with_rows_is_ok() {
    let check = build_statusline_check(&[tier(TierState::Wired)], Ok(12), true);

    assert_eq!(check.status, CheckStatus::Ok);
    assert!(check.message.contains("12 row(s)"), "{}", check.message);
}

/// Why (#7617): no tier wired means Claude Code draws no status bar at all, so
/// the segment's absence is not the segment's fault — and the operator needs to
/// be told which thing to fix.
/// Test: itself.
#[test]
fn no_tier_wired_fails() {
    let check = build_statusline_check(
        &[tier(TierState::Missing), tier(TierState::Missing)],
        Ok(12),
        true,
    );

    assert_eq!(check.status, CheckStatus::Fail);
    assert!(check.message.contains("--fix"), "{}", check.message);
}

/// Why: Claude Code merges tiers, so one stale entry beside a live one decides
/// what renders depending on resolution order — reportable, but not fatal.
/// Test: itself.
#[test]
fn a_stale_tier_beside_a_wired_one_warns() {
    let check = build_statusline_check(
        &[tier(TierState::Wired), tier(TierState::Stale)],
        Ok(12),
        true,
    );

    assert_eq!(check.status, CheckStatus::Warn);
    assert!(
        check.message.contains("no longer on disk"),
        "{}",
        check.message
    );
}

/// Why (Fail-Open Check): the segment's empty state claims no number whether the
/// ledger is empty or unreadable — by design. This check is the only place that
/// distinction exists, so an unreadable ledger must be a Fail and never fold
/// into the same Warn as an empty one.
/// Test: itself.
#[test]
fn an_unreadable_ledger_fails() {
    let check = build_statusline_check(
        &[tier(TierState::Wired)],
        Err("permission denied".to_string()),
        true,
    );

    assert_eq!(check.status, CheckStatus::Fail);
    assert!(
        check.message.contains("permission denied")
            && check.message.contains("repair savings-ledger"),
        "{}",
        check.message
    );
}

/// Why: a fresh install has no rows and that is not a fault — but it IS the
/// reason the bar shows `💸—`, which the operator should be able to read here
/// rather than infer.
/// Test: itself.
#[test]
fn an_absent_ledger_warns() {
    let check = build_statusline_check(&[tier(TierState::Wired)], Ok(0), true);

    assert_eq!(check.status, CheckStatus::Warn);
    assert!(check.message.contains("no rows yet"), "{}", check.message);
}

/// Test: itself.
#[test]
fn an_unlistable_store_fails() {
    let check = build_statusline_check(&[tier(TierState::Wired)], Ok(12), false);

    assert_eq!(check.status, CheckStatus::Fail);
    assert!(
        check.message.contains("cannot be listed"),
        "{}",
        check.message
    );
}

/// Why: an absent `usage/` directory is a fresh install, not a fault; a real
/// directory is listable. Both must read as readable.
/// Test: itself.
#[test]
fn an_absent_or_real_store_is_readable() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(store_is_readable(&dir.path().join("usage")));
    assert!(store_is_readable(dir.path()));
}

/// Why: an absent ledger is zero rows, not an error — the two are different
/// verdicts and `ledger_state` is where they part.
/// Test: itself.
#[test]
fn an_absent_ledger_reads_as_zero_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(ledger_state(&dir.path().join("savings.jsonl")), Ok(0));

    let ledger = dir.path().join("savings.jsonl");
    std::fs::write(&ledger, "{}\n\n{}\n").expect("seed");
    assert_eq!(ledger_state(&ledger), Ok(2));
}
