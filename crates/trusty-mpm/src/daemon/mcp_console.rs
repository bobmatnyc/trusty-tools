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
/// #2196 adds the global `untracked_sync` allowlist section (each `projects`
/// entry already carries its own optional `untracked_sync` override via its
/// own `Serialize` impl, so no separate top-level field is needed for that).
/// What: returns `{ workspace_root_template, auto_resume, default_model, github,
/// untracked_sync, projects, workspace_root }` where `workspace_root` is
/// [`trusty_tools_config::workspace_root`].
/// Test: `config_read_returns_resolved_root`,
/// `config_to_json_includes_github_and_projects`,
/// `config_to_json_includes_untracked_sync`.
fn config_to_json(config: &TrustyToolsConfig) -> Value {
    json!({
        "workspace_root_template": config.workspace_root_template,
        "auto_resume": config.auto_resume,
        "default_model": config.default_model,
        "github": config.github,
        "untracked_sync": config.untracked_sync,
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
/// identity overrides, and — since #2196 — the global/per-project untracked-
/// secret-file sync allowlist, all without touching the legacy
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
    untracked_sync_patterns: Option<Vec<String>>,
    untracked_sync_enabled: Option<bool>,
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
        untracked_sync_patterns,
        untracked_sync_enabled,
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
/// top-level "omitted fields unchanged" contract. `untracked_sync_patterns`/
/// `untracked_sync_enabled` (#2196) follow the SAME `project_name` routing as
/// `github_*` (global tier when `project_name` is `None`, that project's own
/// tier when `Some`) — unlike commit identity, untracked-sync HAS a global
/// tier, so no project_name is required for it.
/// Test: `config_write_merges_top_level_fields`,
/// `config_write_sets_global_github_binding`,
/// `config_write_sets_project_github_and_commit_identity`,
/// `config_write_project_not_found_errors`,
/// `config_write_preserves_omitted_github_subfields`,
/// `config_write_global_commit_identity_rejected`,
/// `config_write_sets_global_untracked_sync`,
/// `config_write_sets_project_untracked_sync`.
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
    untracked_sync_patterns: Option<Vec<String>>,
    untracked_sync_enabled: Option<bool>,
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
    let has_untracked_sync_edit =
        untracked_sync_patterns.is_some() || untracked_sync_enabled.is_some();
    if !has_github_edit && !has_commit_edit && !has_untracked_sync_edit {
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
            if has_untracked_sync_edit {
                config.untracked_sync = Some(merge_untracked_sync_config(
                    config.untracked_sync.take(),
                    untracked_sync_patterns,
                    untracked_sync_enabled,
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
            if has_untracked_sync_edit {
                entry.untracked_sync = Some(merge_untracked_sync_config(
                    entry.untracked_sync.take(),
                    untracked_sync_patterns,
                    untracked_sync_enabled,
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

/// Overlay supplied `untracked_sync_*` fields onto an existing (or absent)
/// [`trusty_tools_config::UntrackedSyncConfig`] (#2196), keeping an omitted
/// field unchanged.
///
/// Why: mirrors [`merge_github_config`]'s "only overlay what was supplied"
/// rule so both `apply_config_write` branches (global/per-project) stay in
/// lockstep.
/// What: starts from `existing.unwrap_or_default()`, overlays `patterns`/
/// `enabled` when `Some`, and returns the result.
/// Test: `config_write_sets_global_untracked_sync`,
/// `config_write_sets_project_untracked_sync`.
fn merge_untracked_sync_config(
    existing: Option<trusty_tools_config::UntrackedSyncConfig>,
    patterns: Option<Vec<String>>,
    enabled: Option<bool>,
) -> trusty_tools_config::UntrackedSyncConfig {
    let mut cfg = existing.unwrap_or_default();
    if let Some(p) = patterns {
        cfg.patterns = Some(p);
    }
    if let Some(e) = enabled {
        cfg.enabled = Some(e);
    }
    cfg
}

#[cfg(test)]
mod tests;
