//! Daemon-side implementation of the console-facing MCP tools (#1222 / P2).
//!
//! Why: trusty-console renders the Sessions tab natively by polling the daemon
//! over MCP (per the #1104 HTTP-only-in-console principle). It needs three tools
//! the existing catalog did not provide: `console_metrics` (the standard
//! [`trusty_common::console_metrics::ConsoleMetricsReport`] every trusty service
//! exposes so the console poller is service-agnostic), `supervisor_status` (the
//! fleet-state snapshot plus the auto-resume control state), and `auto_resume_set`
//! (the console's non-CLI control for enabling/disabling auto-resume — RFC §6 Q6).
//! Putting them here keeps `mcp_backend.rs` under the 500-SLOC production cap.
//! What: three free async functions over `&Arc<DaemonState>`:
//! [`console_metrics`] builds a report whose `metrics` payload carries the
//! `FleetMetrics` and supervisor flags; [`supervisor_status`] returns the same
//! fleet snapshot as a bare object; [`auto_resume_set`] persists the operator's
//! desired flag and echoes the resulting state. All return the same JSON shapes
//! across MCP and any future HTTP transport.
//! Test: `cargo test -p trusty-mpm daemon::mcp_console` covers report shape,
//! fleet derivation, and the auto-resume persistence round-trip.

use std::sync::Arc;

use serde_json::{Value, json};
use trusty_common::console_metrics::{ConsoleMetricsReport, ServiceHealth, make_report};

use crate::core::auto_resume;
use crate::core::trusty_tools_config::{self, TrustyToolsConfig};
use crate::daemon::state::DaemonState;
use crate::supervisor::metrics::FleetMetrics;

/// Schema version of the `console_metrics` `metrics` payload for trusty-mpm.
///
/// Why: the console bumps its UI when this changes; starting at 1 establishes the
/// contract for the Sessions tab.
/// What: monotonically-increasing integer carried in every report.
/// Test: asserted in `console_metrics_report_has_expected_shape`.
const METRICS_SCHEMA_VERSION: u32 = 1;

/// Service version reported in `console_metrics` (the crate semver).
const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Build the supervisor/auto-resume + fleet snapshot shared by both
/// `console_metrics` and `supervisor_status`.
///
/// Why: both tools surface the same fleet counts and auto-resume state; deriving
/// them in one place keeps the two tool payloads in lockstep.
/// What: lists the managed session records, derives [`FleetMetrics`] from them,
/// and reads the persisted desired + live env auto-resume flags. Returns a JSON
/// object: `{ fleet, auto_resume: { desired, env, pending_restart } }`.
///
/// Auto-resume control fields:
/// - `desired`: the operator's persisted choice (console-mutable, the source of
///   truth the supervisor will read on its next sweep).
/// - `env`: the flag the supervisor process actually booted with
///   (`TRUSTY_MPM_AUTO_RESUME`); changes only take full effect on restart.
/// - `pending_restart`: `desired != env` — render a "restart pending" hint.
///
/// There is deliberately NO `effective` field: until the supervisor-sweep wiring
/// lands, "what is in force right now" is exactly `env`, so a separate
/// `effective` field would just duplicate `env` and mislead readers into thinking
/// it already reflects the desired-state file. Reintroduce it (distinct from
/// `env`) only when the supervisor honours `desired` mid-run.
/// Test: `supervisor_status_reports_fleet_and_auto_resume`.
async fn fleet_snapshot(state: &Arc<DaemonState>) -> Value {
    let mgr = state.session_manager().await;
    let records = mgr.list().await;
    let fleet = FleetMetrics::from_records(&records);

    // The persisted desired flag is the console-mutable control; the env flag is
    // what the supervisor process booted with. They can disagree until restart.
    let desired = auto_resume::read_desired().unwrap_or(false);
    let env = auto_resume::effective_from_env();

    json!({
        "fleet": fleet,
        "auto_resume": {
            "desired": desired,
            "env": env,
            "pending_restart": desired != env,
        },
    })
}

/// Back the `console_metrics` tool with a [`ConsoleMetricsReport`].
///
/// Why: the trusty-console metrics poller calls `console_metrics` on every
/// service uniformly; trusty-mpm must speak the same contract so it appears in
/// the dashboard like the other services.
/// What: classifies health (`Ok` normally, `Degraded` when any session is
/// `errored`), packs the fleet + auto-resume snapshot into the report's flexible
/// `metrics` field, and returns it as a JSON object the console deserialises with
/// `trusty_common::console_metrics::parse_report`.
/// Test: `console_metrics_report_has_expected_shape`.
pub async fn console_metrics(state: &Arc<DaemonState>) -> Result<Value, String> {
    let snapshot = fleet_snapshot(state).await;
    let errored = snapshot
        .get("fleet")
        .and_then(|f| f.get("errored"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let status = if errored > 0 {
        ServiceHealth::Degraded
    } else {
        ServiceHealth::Ok
    };

    let report: ConsoleMetricsReport = make_report(
        "trusty-mpm",
        "Trusty MPM",
        SERVICE_VERSION,
        status,
        snapshot,
        METRICS_SCHEMA_VERSION,
    );

    serde_json::to_value(&report).map_err(|e| format!("serialising console_metrics report: {e}"))
}

/// Back the `supervisor_status` tool with the fleet + auto-resume snapshot.
///
/// Why: the Sessions tab's supervisor widget needs fleet counts by lifecycle
/// state and the current auto-resume control state in one call, without parsing
/// the full `console_metrics` envelope.
/// What: returns the bare `{ fleet, auto_resume }` object from [`fleet_snapshot`].
/// Test: `supervisor_status_reports_fleet_and_auto_resume`.
pub async fn supervisor_status(state: &Arc<DaemonState>) -> Result<Value, String> {
    Ok(fleet_snapshot(state).await)
}

/// Back the `auto_resume_set` tool: persist the operator's desired flag.
///
/// Why: the console toggle must durably record whether auto-resume should be on
/// (RFC §6 Q6 — controls live in the console, not CLI-only). The supervisor runs
/// as a separate process, so this writes the desired state the supervisor reads
/// on its next sweep rather than mutating a live env var.
/// What: writes `~/.trusty-mpm/auto_resume`, then echoes the resulting
/// `{ desired, env, pending_restart }` so the console can render the toggle and a
/// "restart pending" hint when the persisted desire differs from the supervisor's
/// boot-time env. (No `effective` field — see [`fleet_snapshot`] for why it would
/// merely duplicate `env` and mislead until the supervisor-sweep wiring lands.)
/// Test: `auto_resume_set_persists_desired`.
pub async fn auto_resume_set(enabled: bool) -> Result<Value, String> {
    auto_resume::write_desired(enabled)
        .map_err(|e| format!("persisting auto_resume desired state: {e}"))?;

    let desired = enabled;
    let env = auto_resume::effective_from_env();
    Ok(json!({
        "desired": desired,
        "env": env,
        "pending_restart": desired != env,
    }))
}

/// Serialise a [`TrustyToolsConfig`] into the JSON the console Config tab renders,
/// annotated with the resolved absolute workspace root.
///
/// Why: both `config_read` and `config_write` return the same shape — the raw
/// config fields plus the `workspace_root` the resolver would actually use — so the
/// UI can show the effective path even when the template field is null. One helper
/// keeps the two tools in lockstep. #2184 adds the global `github` binding and the
/// full `projects` list (each entry now carries its own optional `github`/
/// `commit_name`/`commit_email` per-project override) so the Config tab can render
/// and edit them without a separate MCP round-trip or hand-editing the YAML.
/// What: returns `{ workspace_root_template, auto_resume, default_model, github,
/// projects, workspace_root }` where `workspace_root` is
/// [`trusty_tools_config::workspace_root`].
/// Test: `config_read_returns_resolved_root`,
/// `config_to_json_includes_github_and_projects`.
fn config_to_json(config: &TrustyToolsConfig) -> Value {
    json!({
        "workspace_root_template": config.workspace_root_template,
        "auto_resume": config.auto_resume,
        "default_model": config.default_model,
        "github": config.github,
        "projects": config.projects,
        "workspace_root": trusty_tools_config::workspace_root(config).to_string_lossy(),
    })
}

/// Back the `config_read` tool: load and return the config-convention file.
///
/// Why: the console Config tab (#1220) reads the current
/// `~/.trusty-tools/trusty-mpm/config.yaml` to render its form.
/// What: loads [`TrustyToolsConfig`] (absent file → defaults) and serialises it via
/// [`config_to_json`], including the resolved absolute workspace root.
/// Test: `config_read_returns_resolved_root`.
pub fn config_read() -> Result<Value, String> {
    Ok(config_to_json(&TrustyToolsConfig::load()))
}

/// Back the `config_write` tool: merge edits and persist the config file.
///
/// Why: the console Config tab's save action durably records the operator's
/// workspace-root / auto-resume / default-model choices (#1220), and — since
/// #2184 — the global GitHub identity binding plus per-project GitHub/commit
/// identity overrides, all without touching the legacy
/// `~/.trusty-mpm/config.toml` or requiring the operator to hand-edit YAML.
/// What: loads the current config, applies [`apply_config_write`] (the
/// testable merge step), writes it back via
/// [`trusty_common::crate_config::save`], and returns the merged config (with
/// the resolved root) on success.
/// Test: `config_write_merges_and_persists` covers `apply_config_write`
/// directly (no real filesystem I/O against the operator's real config file —
/// see that function's own doc for why `config_write` itself is not unit
/// tested against the real file).
#[allow(clippy::too_many_arguments)]
pub fn config_write(
    workspace_root_template: Option<&str>,
    auto_resume: Option<bool>,
    default_model: Option<&str>,
    project_name: Option<&str>,
    github_config_dir: Option<&str>,
    github_token_env: Option<&str>,
    github_account: Option<&str>,
    github_host: Option<&str>,
    commit_name: Option<&str>,
    commit_email: Option<&str>,
) -> Result<Value, String> {
    let mut config = TrustyToolsConfig::load();
    apply_config_write(
        &mut config,
        workspace_root_template,
        auto_resume,
        default_model,
        project_name,
        github_config_dir,
        github_token_env,
        github_account,
        github_host,
        commit_name,
        commit_email,
    )?;

    trusty_common::crate_config::save(trusty_tools_config::CRATE_NAME, &config)
        .map_err(|e| format!("persisting trusty-mpm config: {e}"))?;

    Ok(config_to_json(&config))
}

/// The pure merge step behind `config_write` (#2184).
///
/// Why: `config_write` persists to the operator's REAL
/// `~/.trusty-tools/trusty-mpm/config.yaml` (there is no dependency-injected
/// base directory in production), so unit-testing `config_write` itself would
/// mutate the developer's actual config file on every `cargo test` run.
/// Extracting the merge decision into a pure `&mut TrustyToolsConfig` function
/// makes the actual field-merge logic — including the #2184 `project_name`
/// routing — fully unit-testable in memory, with `config_write` reduced to
/// "load, merge, save".
/// What: when `workspace_root_template`/`auto_resume`/`default_model` are
/// `Some`, they overlay the corresponding TOP-LEVEL field (unchanged
/// behaviour, #1220). When `project_name` is `None`, the `github_*` args
/// overlay the GLOBAL `config.github` tier (creating it if absent and at
/// least one `github_*` field is supplied); `commit_*` args with no
/// `project_name` are rejected (#2184 defines commit identity as
/// project-scoped only). When `project_name` is `Some(name)`, the `github_*`
/// AND `commit_*` args overlay the MATCHING entry in `config.projects`
/// instead — `Err` when no entry with that `name` already exists
/// (`config_write` deliberately does not fabricate a `ProjectConfig` with an
/// empty `repo_url`; register the project first via `project_register` or a
/// static `config.projects` entry). Each `github_*` field only overlays its
/// own sub-field — omitted sub-fields keep their previous value, matching the
/// top-level "omitted fields unchanged" contract.
/// Test: `config_write_merges_top_level_fields`,
/// `config_write_sets_global_github_binding`,
/// `config_write_sets_project_github_and_commit_identity`,
/// `config_write_project_not_found_errors`,
/// `config_write_preserves_omitted_github_subfields`,
/// `config_write_global_commit_identity_rejected`.
#[allow(clippy::too_many_arguments)]
fn apply_config_write(
    config: &mut TrustyToolsConfig,
    workspace_root_template: Option<&str>,
    auto_resume: Option<bool>,
    default_model: Option<&str>,
    project_name: Option<&str>,
    github_config_dir: Option<&str>,
    github_token_env: Option<&str>,
    github_account: Option<&str>,
    github_host: Option<&str>,
    commit_name: Option<&str>,
    commit_email: Option<&str>,
) -> Result<(), String> {
    if let Some(t) = workspace_root_template {
        config.workspace_root_template = Some(t.to_string());
    }
    if let Some(a) = auto_resume {
        config.auto_resume = Some(a);
    }
    if let Some(m) = default_model {
        config.default_model = Some(m.to_string());
    }

    let has_github_edit = github_config_dir.is_some()
        || github_token_env.is_some()
        || github_account.is_some()
        || github_host.is_some();
    let has_commit_edit = commit_name.is_some() || commit_email.is_some();
    if !has_github_edit && !has_commit_edit {
        return Ok(());
    }

    match project_name {
        None => {
            if has_github_edit {
                config.github = Some(merge_github_config(
                    config.github.take(),
                    github_config_dir,
                    github_token_env,
                    github_account,
                    github_host,
                ));
            }
            // #2184 defines commit identity as PROJECT-scoped only (no global
            // tier) — a caller supplying commit_name/commit_email with no
            // project_name has nowhere to persist them.
            if has_commit_edit {
                return Err(
                    "commit_name/commit_email require project_name (commit identity is \
                     project-scoped; there is no global commit identity tier)"
                        .to_string(),
                );
            }
        }
        Some(name) => {
            let entry = config
                .projects
                .iter_mut()
                .find(|p| p.name == name)
                .ok_or_else(|| {
                    format!(
                        "project '{name}' not found in config.projects; register it first \
                         (e.g. via `project_register` or a static config.yaml entry) before \
                         setting its github/commit identity"
                    )
                })?;
            if has_github_edit {
                entry.github = Some(merge_github_config(
                    entry.github.take(),
                    github_config_dir,
                    github_token_env,
                    github_account,
                    github_host,
                ));
            }
            if let Some(n) = commit_name {
                entry.commit_name = Some(n.to_string());
            }
            if let Some(e) = commit_email {
                entry.commit_email = Some(e.to_string());
            }
        }
    }

    Ok(())
}

/// Overlay supplied `github_*` sub-fields onto an existing (or absent)
/// [`trusty_tools_config::GithubConfig`], keeping omitted sub-fields unchanged.
///
/// Why: shared by both the global and per-project branches of
/// [`apply_config_write`] so the "only overlay what was supplied" rule cannot
/// drift between the two.
/// What: starts from `existing.unwrap_or_default()`, overlays each `Some`
/// argument onto the matching field, and returns the result (never
/// meaningfully empty — the caller only invokes this when at least one
/// `github_*` field was supplied).
/// Test: `config_write_preserves_omitted_github_subfields`.
fn merge_github_config(
    existing: Option<trusty_tools_config::GithubConfig>,
    config_dir: Option<&str>,
    token_env: Option<&str>,
    account: Option<&str>,
    host: Option<&str>,
) -> trusty_tools_config::GithubConfig {
    let mut cfg = existing.unwrap_or_default();
    if let Some(d) = config_dir {
        cfg.config_dir = Some(d.into());
    }
    if let Some(t) = token_env {
        cfg.token_env = Some(t.to_string());
    }
    if let Some(a) = account {
        cfg.account = Some(a.to_string());
    }
    if let Some(h) = host {
        cfg.host = Some(h.to_string());
    }
    cfg
}

#[cfg(test)]
mod tests {
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
            got["workspace_root"].is_string()
                && !got["workspace_root"].as_str().unwrap().is_empty(),
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
                github: None,
                commit_name: Some("Bot".into()),
                commit_email: None,
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
            github: None,
            commit_name: None,
            commit_email: None,
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
}
