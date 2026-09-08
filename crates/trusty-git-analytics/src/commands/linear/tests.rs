//! Tests for `tga linear sync` / `tga linear freshness` (issue #7139).
//!
//! Bulk-page fetch, pagination, timestamp mapping and empty-team coverage
//! live in `collect::linear::client::tests::bulk_sync` (mock HTTP, no real
//! network) — this module covers what sits above the HTTP layer: team-key
//! resolution and freshness reporting against an in-memory database.

use super::*;
use tga::core::config::LinearConfig;
use tga::core::db::Database;

fn config_with_teams(teams: &[&str]) -> Config {
    Config {
        linear: Some(LinearConfig {
            team_keys: teams.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[test]
fn resolve_team_key_prefers_the_explicit_cli_flag() {
    let config = config_with_teams(&["ENG", "FE"]);
    let key = resolve_team_key(&config, Some("OPS")).expect("resolves");
    assert_eq!(key, "OPS");
}

#[test]
fn resolve_team_key_falls_back_to_a_single_configured_team() {
    let config = config_with_teams(&["ENG"]);
    let key = resolve_team_key(&config, None).expect("resolves");
    assert_eq!(key, "ENG");
}

#[test]
fn resolve_team_key_rejects_an_ambiguous_multi_team_config() {
    let config = config_with_teams(&["ENG", "FE"]);
    let err = resolve_team_key(&config, None).expect_err("ambiguous");
    assert!(err.to_string().contains("ambiguous"), "{err}");
}

#[test]
fn resolve_team_key_rejects_no_scope_at_all() {
    let config = config_with_teams(&[]);
    let err = resolve_team_key(&config, None).expect_err("no scope");
    assert!(err.to_string().contains("--team"), "{err}");
}

#[test]
fn resolve_team_key_rejects_a_malformed_cli_team() {
    let config = config_with_teams(&[]);
    let err = resolve_team_key(&config, Some("1BAD")).expect_err("malformed");
    assert!(err.to_string().contains("invalid Linear team key"), "{err}");
}

/// Deliverable #7139.3: freshness fails loudly when a team has never synced
/// — no `linear_sync_cursor` row at all.
#[test]
fn freshness_fails_when_a_team_has_never_synced() {
    let db = Database::open_in_memory().expect("open");
    let config = config_with_teams(&["ENG"]);
    let args = LinearFreshnessArgs {
        max_age_days: 2,
        report_only: false,
        team: None,
    };
    let err = run_freshness(&config, &db, args).expect_err("never synced is stale");
    assert!(
        err.to_string().contains("never synced") || err.to_string().contains("stale"),
        "{err}"
    );
}

/// Deliverable #7139.3: freshness reports the last sync time and passes
/// when the recorded run is within the threshold.
#[test]
fn freshness_reports_the_last_sync_time_and_passes_when_fresh() {
    let db = Database::open_in_memory().expect("open");
    tga::core::db::set_linear_cursor(db.connection(), "ENG", "2026-01-01T00:00:00+00:00", 7)
        .expect("record a sync");

    let config = config_with_teams(&["ENG"]);
    let args = LinearFreshnessArgs {
        max_age_days: 3650, // far future threshold so "just synced" always passes
        report_only: false,
        team: None,
    };
    run_freshness(&config, &db, args).expect("a just-recorded sync is fresh");
}

/// A stale cursor fails the check even though it exists — the freshness
/// verdict is about recency, not mere presence.
#[test]
fn freshness_fails_when_the_recorded_run_is_older_than_the_threshold() {
    let db = Database::open_in_memory().expect("open");
    // `set_linear_cursor` always stamps `last_run_at` with "now", so exercise
    // staleness via a threshold of -1 days: any `last_run_at`, however
    // recent, is already older than "minus one day from now".
    tga::core::db::set_linear_cursor(db.connection(), "ENG", "2026-01-01T00:00:00+00:00", 3)
        .expect("record a sync");

    let config = config_with_teams(&["ENG"]);
    let args = LinearFreshnessArgs {
        max_age_days: -1,
        report_only: false,
        team: None,
    };
    let err = run_freshness(&config, &db, args).expect_err("stale by threshold");
    assert!(err.to_string().contains("stale"), "{err}");
}

/// `--report-only` never fails the process, even when every scope is stale.
#[test]
fn freshness_report_only_never_fails() {
    let db = Database::open_in_memory().expect("open");
    let config = config_with_teams(&["ENG"]);
    let args = LinearFreshnessArgs {
        max_age_days: 2,
        report_only: true,
        team: None,
    };
    run_freshness(&config, &db, args).expect("report-only always returns Ok");
}

/// With no `--team`, no configured `linear.team_keys`, and no recorded
/// cursor, there is nothing to check — an explicit error, not a silent pass.
#[test]
fn freshness_with_nothing_configured_and_nothing_recorded_is_an_error() {
    let db = Database::open_in_memory().expect("open");
    let config = config_with_teams(&[]);
    let args = LinearFreshnessArgs {
        max_age_days: 2,
        report_only: false,
        team: None,
    };
    let err = run_freshness(&config, &db, args).expect_err("nothing to check");
    assert!(err.to_string().contains("no Linear team"), "{err}");
}
