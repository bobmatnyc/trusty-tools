//! Unit tests for [`super`] (the console-facing MCP tool implementations).
//!
//! Why: split out of `mcp_console.rs` so the production module stays under
//! the 500-SLOC cap (mirrors `inproject.rs`/`inproject/tests.rs`); the tests
//! themselves are unchanged.
//! What: `config_read`/`config_write`/`apply_config_write` shape and merge
//! tests (including the #2196 `untracked_sync` cases), plus the
//! `console_metrics`/`supervisor_status` report-shape tests.
//! Test: this IS the test module.

use super::*;

fn state() -> Arc<DaemonState> {
    DaemonState::shared()
}

/// Why: `config_read` must always return the four-field shape including the
/// resolved absolute `workspace_root`, even on a fresh install (defaults).
/// Test: this test (reads the real/default config; asserts SHAPE, not values,
/// since the host may have a real config file).
#[tokio::test]
async fn config_read_returns_resolved_root() {
    let got = config_read().expect("config_read");
    assert!(
        got.get("workspace_root_template").is_some(),
        "must carry workspace_root_template key: {got}"
    );
    assert!(
        got["workspace_root"].is_string() && !got["workspace_root"].as_str().unwrap().is_empty(),
        "workspace_root must be a non-empty resolved path: {got}"
    );
}

/// Why: `config_to_json` must surface the #2184 `github`/`projects` fields
/// so the Config tab can render/edit them without a separate round-trip.
/// Test: this test.
#[test]
fn config_to_json_includes_github_and_projects() {
    let config = TrustyToolsConfig {
        github: Some(trusty_tools_config::GithubConfig {
            config_dir: Some("/cfg/global".into()),
            token_env: None,
            account: None,
            host: None,
        }),
        projects: vec![trusty_tools_config::ProjectConfig {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: None,
            stack_hint: None,
            tags: None,
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: Some("Bot".into()),
            commit_email: None,
            untracked_sync: None,
        }],
        ..Default::default()
    };
    let json = config_to_json(&config);
    assert_eq!(json["github"]["config_dir"], "/cfg/global");
    assert_eq!(json["projects"][0]["name"], "widget");
    assert_eq!(json["projects"][0]["commit_name"], "Bot");
}

// ── apply_config_write (#2184) — pure merge logic, no real config file I/O ──

fn project(name: &str) -> trusty_tools_config::ProjectConfig {
    trusty_tools_config::ProjectConfig {
        name: name.to_string(),
        repo_url: format!("https://github.com/acme/{name}"),
        default_branch: None,
        stack_hint: None,
        tags: None,
        description: None,
        gh_user: None,
        gh_account: None,
        github: None,
        commit_name: None,
        commit_email: None,
        untracked_sync: None,
    }
}

/// Why: the pre-#2184 top-level fields must still merge exactly as before
/// (no regression from the new params).
/// Test: this test.
#[test]
fn config_write_merges_top_level_fields() {
    let mut config = TrustyToolsConfig::default();
    apply_config_write(
        &mut config,
        Some("~/custom-projects"),
        Some(true),
        Some("opus"),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .expect("ok");
    assert_eq!(
        config.workspace_root_template.as_deref(),
        Some("~/custom-projects")
    );
    assert_eq!(config.auto_resume, Some(true));
    assert_eq!(config.default_model.as_deref(), Some("opus"));
    assert_eq!(config.github, None, "no github edit supplied");
}

/// Why: with no `project_name`, `github_*` fields must set the GLOBAL
/// `config.github` tier.
/// Test: this test.
#[test]
fn config_write_sets_global_github_binding() {
    let mut config = TrustyToolsConfig::default();
    apply_config_write(
        &mut config,
        None,
        None,
        None,
        None,
        Some("/cfg/global"),
        None,
        None,
        Some("github.example.com"),
        None,
        None,
        None,
        None,
    )
    .expect("ok");
    let gh = config.github.expect("global github set");
    assert_eq!(
        gh.config_dir.as_deref(),
        Some(std::path::Path::new("/cfg/global"))
    );
    assert_eq!(gh.host.as_deref(), Some("github.example.com"));
}

/// Why: with `project_name` set, `github_*`/`commit_*` fields must
/// overlay the MATCHING `config.projects` entry, not the global tier.
/// Test: this test.
#[test]
fn config_write_sets_project_github_and_commit_identity() {
    let mut config = TrustyToolsConfig {
        projects: vec![project("widget")],
        ..Default::default()
    };
    apply_config_write(
        &mut config,
        None,
        None,
        None,
        Some("widget"),
        Some("/cfg/widget"),
        None,
        Some("bob-work"),
        None,
        Some("Widget Bot"),
        Some("widget-bot@example.com"),
        None,
        None,
    )
    .expect("ok");
    assert_eq!(config.github, None, "global tier must be untouched");
    let entry = &config.projects[0];
    let gh = entry.github.as_ref().expect("project github set");
    assert_eq!(
        gh.config_dir.as_deref(),
        Some(std::path::Path::new("/cfg/widget"))
    );
    assert_eq!(gh.account.as_deref(), Some("bob-work"));
    assert_eq!(entry.commit_name.as_deref(), Some("Widget Bot"));
    assert_eq!(
        entry.commit_email.as_deref(),
        Some("widget-bot@example.com")
    );
}

/// Why: setting a project's identity for a `project_name` that is NOT
/// already declared in `config.projects` must error, never fabricate a
/// `ProjectConfig` with an empty `repo_url`.
/// Test: this test.
#[test]
fn config_write_project_not_found_errors() {
    let mut config = TrustyToolsConfig::default();
    let err = apply_config_write(
        &mut config,
        None,
        None,
        None,
        Some("does-not-exist"),
        Some("/cfg/x"),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap_err();
    assert!(err.contains("does-not-exist"), "err: {err}");
    assert!(err.contains("not found"), "err: {err}");
}

/// Why: `merge_github_config` must overlay only the SUPPLIED sub-fields,
/// preserving whatever was already set for the omitted ones — matching
/// the top-level "omitted fields unchanged" contract.
/// Test: this test.
#[test]
fn config_write_preserves_omitted_github_subfields() {
    let mut config = TrustyToolsConfig {
        github: Some(trusty_tools_config::GithubConfig {
            config_dir: Some("/cfg/original".into()),
            token_env: None,
            account: Some("original-account".into()),
            host: None,
        }),
        ..Default::default()
    };
    // Only update `host`; config_dir/account must survive untouched.
    apply_config_write(
        &mut config,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some("github.example.com"),
        None,
        None,
        None,
        None,
    )
    .expect("ok");
    let gh = config.github.expect("github still set");
    assert_eq!(
        gh.config_dir.as_deref(),
        Some(std::path::Path::new("/cfg/original"))
    );
    assert_eq!(gh.account.as_deref(), Some("original-account"));
    assert_eq!(gh.host.as_deref(), Some("github.example.com"));
}

/// Why: commit identity is project-scoped only (#2184) — supplying
/// `commit_name`/`commit_email` with no `project_name` must be rejected
/// rather than silently discarded or persisted somewhere unexpected.
/// Test: this test.
#[test]
fn config_write_global_commit_identity_rejected() {
    let mut config = TrustyToolsConfig::default();
    let err = apply_config_write(
        &mut config,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some("Global Bot"),
        None,
        None,
        None,
    )
    .unwrap_err();
    assert!(err.contains("project_name"), "err: {err}");
}

/// Why: the doc-referenced round-trip contract — every supplied field
/// makes it into the merged config and NOTHING supplied is silently
/// dropped. Exercises the full param list together (the shape
/// `config_write` itself passes through to `apply_config_write`).
/// Test: this test.
#[test]
fn config_write_merges_and_persists() {
    let mut config = TrustyToolsConfig {
        projects: vec![project("widget")],
        ..Default::default()
    };
    apply_config_write(
        &mut config,
        Some("~/custom-projects"),
        Some(false),
        Some("sonnet"),
        Some("widget"),
        Some("/cfg/widget"),
        Some("WIDGET_GH_TOKEN"),
        None,
        Some("github.example.com"),
        Some("Widget Bot"),
        Some("widget-bot@example.com"),
        None,
        None,
    )
    .expect("ok");
    assert_eq!(
        config.workspace_root_template.as_deref(),
        Some("~/custom-projects")
    );
    assert_eq!(config.auto_resume, Some(false));
    assert_eq!(config.default_model.as_deref(), Some("sonnet"));
    let entry = &config.projects[0];
    let gh = entry.github.as_ref().expect("project github set");
    assert_eq!(gh.token_env.as_deref(), Some("WIDGET_GH_TOKEN"));
    assert_eq!(gh.host.as_deref(), Some("github.example.com"));
    assert_eq!(entry.commit_name.as_deref(), Some("Widget Bot"));
}

/// Why (#2196): `config_to_json` must surface the global `untracked_sync`
/// section so the Config tab can render/edit it without a separate
/// round-trip.
/// Test: this test.
#[test]
fn config_to_json_includes_untracked_sync() {
    let config = TrustyToolsConfig {
        untracked_sync: Some(trusty_tools_config::UntrackedSyncConfig {
            patterns: Some(vec![".env".into(), ".env.*".into()]),
            enabled: Some(false),
        }),
        ..Default::default()
    };
    let json = config_to_json(&config);
    assert_eq!(json["untracked_sync"]["enabled"], false);
    assert_eq!(json["untracked_sync"]["patterns"][0], ".env");
}

/// Why (#2196): with no `project_name`, `untracked_sync_*` fields must
/// set the GLOBAL `config.untracked_sync` tier (mirrors
/// `config_write_sets_global_github_binding`).
/// Test: this test.
#[test]
fn config_write_sets_global_untracked_sync() {
    let mut config = TrustyToolsConfig::default();
    apply_config_write(
        &mut config,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(vec![".env".to_string(), ".env.local".to_string()]),
        Some(false),
    )
    .expect("ok");
    let cfg = config.untracked_sync.expect("global untracked_sync set");
    assert_eq!(
        cfg.patterns,
        Some(vec![".env".to_string(), ".env.local".to_string()])
    );
    assert_eq!(cfg.enabled, Some(false));
}

/// Why (#2196): with `project_name` set, `untracked_sync_*` fields must
/// overlay the MATCHING `config.projects` entry, not the global tier —
/// and omitted sub-fields (e.g. `enabled` here) must survive untouched.
/// Test: this test.
#[test]
fn config_write_sets_project_untracked_sync() {
    let mut config = TrustyToolsConfig {
        projects: vec![project("widget")],
        ..Default::default()
    };
    apply_config_write(
        &mut config,
        None,
        None,
        None,
        Some("widget"),
        None,
        None,
        None,
        None,
        None,
        None,
        Some(vec![".env.widget-only".to_string()]),
        None,
    )
    .expect("ok");
    assert_eq!(config.untracked_sync, None, "global tier must be untouched");
    let entry = &config.projects[0];
    let cfg = entry
        .untracked_sync
        .as_ref()
        .expect("project untracked_sync set");
    assert_eq!(cfg.patterns, Some(vec![".env.widget-only".to_string()]));
    assert_eq!(
        cfg.enabled, None,
        "enabled was not supplied; must stay None"
    );
}

/// Why: the console deserialises the report with `parse_report`; the tool must
/// return the exact `ConsoleMetricsReport` shape (service_id, status, and a
/// `metrics.fleet` object).
/// Test: this test.
// NOTE: `DaemonState::shared()` is a PROCESS-WIDE singleton, so other tests in
// this binary may have registered managed sessions before these run. The
// assertions below therefore check SHAPE and field TYPES, never exact fleet
// counts (which are non-deterministic across the shared test process).

#[tokio::test]
async fn console_metrics_report_has_expected_shape() {
    let report = console_metrics(&state()).await.expect("report");
    assert_eq!(report["service_id"], "trusty-mpm");
    assert_eq!(report["display_name"], "Trusty MPM");
    assert_eq!(report["metrics_schema_version"], METRICS_SCHEMA_VERSION);
    // Status is one of the coarse ServiceHealth variants.
    assert!(
        matches!(report["status"].as_str(), Some("ok") | Some("degraded")),
        "status must be ok|degraded: {report}"
    );
    assert!(
        report["metrics"]["fleet"].is_object(),
        "metrics.fleet must be an object: {report}"
    );
    assert!(
        report["metrics"]["fleet"]["total"].is_u64(),
        "metrics.fleet.total must be an integer: {report}"
    );
    assert!(
        report["metrics"]["auto_resume"].is_object(),
        "metrics.auto_resume must be an object: {report}"
    );
}

/// Why: the supervisor widget reads `fleet` counts and the auto-resume control
/// state from this object; both must be present and well-typed.
/// Test: this test.
#[tokio::test]
async fn supervisor_status_reports_fleet_and_auto_resume() {
    let status = supervisor_status(&state()).await.expect("status");
    // Fleet counts must be present and integer-typed (exact values are
    // non-deterministic in the shared-singleton test process).
    assert!(status["fleet"]["active"].is_u64());
    assert!(status["fleet"]["stopped"].is_u64());
    assert!(status["fleet"]["total"].is_u64());
    // Auto-resume control block must carry the three flags.
    assert!(status["auto_resume"]["desired"].is_boolean());
    assert!(status["auto_resume"]["env"].is_boolean());
    assert!(status["auto_resume"]["pending_restart"].is_boolean());
}
