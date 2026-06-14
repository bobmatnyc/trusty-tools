//! [`ClaudeConfigAnalyzer`] — filesystem reads, merge, and analysis.
//!
//! Why: isolated from the checkpointer and profile-deployer so each submodule
//! stays under the 500-SLOC cap and has a single responsibility.
//! What: reads + merges the four settings files into a [`ClaudeConfig`] and
//! turns that into a ranked [`Vec<ConfigRecommendation>`].
//! Test: see `super::tests`.

use std::path::Path;

use serde_json::Value;

use crate::core::claude_config::{ClaudeConfig, ClaudeConfigPaths, ConfigRecommendation, Severity};
use crate::core::{Error, Result};

use super::checkpointer::ConfigCheckpointer;

/// Reads, analyzes, and edits Claude Code configuration on disk.
///
/// Why: a unit type groups the I/O operations that act on a
/// [`ClaudeConfigPaths`]; none of them needs instance state.
/// What: `read_config` merges the four settings files into a [`ClaudeConfig`],
/// `analyze` turns that into recommendations, `apply_recommendation` writes the
/// fix back to disk.
/// Test: `read_config_detects_hooks`, `analyze_flags_missing_hooks`,
/// `apply_add_hooks_writes_settings`.
pub struct ClaudeConfigAnalyzer;

impl ClaudeConfigAnalyzer {
    /// Read and merge a project's Claude Code settings into a [`ClaudeConfig`].
    ///
    /// Why: recommendations are derived from a few high-level facts spread
    /// across four JSON files and two agent directories; merging them once
    /// keeps `analyze` simple.
    /// What: reads each settings file (missing files contribute nothing),
    /// OR-merges the `hooks` / `permissions.allow` / `env` facts, and scans the
    /// agent directories for `*.md` files. Never fails — an unreadable or
    /// malformed file is logged and skipped.
    /// Test: `read_config_detects_hooks`, `read_config_missing_files_is_empty`.
    pub fn read_config(paths: &ClaudeConfigPaths) -> ClaudeConfig {
        let mut config = ClaudeConfig::default();
        for settings_path in [
            &paths.user_settings,
            &paths.user_local_settings,
            &paths.project_settings,
            &paths.project_local_settings,
        ] {
            if let Some(json) = read_json(settings_path) {
                merge_settings(&mut config, &json);
            }
        }
        config.has_agents = dir_has_agent_files(&paths.user_agents_dir)
            || dir_has_agent_files(&paths.project_agents_dir);
        config
    }

    /// Produce config recommendations for an analyzed [`ClaudeConfig`].
    ///
    /// Why: trusty-mpm proactively surfaces config gaps — missing oversight
    /// hooks, an overly broad permission allow list, no deployed agents, a
    /// missing API key — so the operator can act on them.
    /// What: returns one [`ConfigRecommendation`] per detected issue, ordered
    /// most-severe first (Critical → Warning → Info) so the dashboard surfaces
    /// security issues at the top; an already-healthy config yields an empty
    /// list.
    /// Test: `analyze_flags_missing_hooks`, `analyze_flags_wildcard`,
    /// `analyze_clean_config_is_empty`, `analyze_partial_config_multiple_recs`.
    pub fn analyze(config: &ClaudeConfig) -> Vec<ConfigRecommendation> {
        let mut recs = Vec::new();

        if !config.has_hooks {
            recs.push(ConfigRecommendation {
                id: "add-trusty-hooks".into(),
                severity: Severity::Warning,
                title: "No hooks configured".into(),
                description: "Claude Code has no hooks. Add pre/post tool-use \
hooks so trusty-mpm can observe and oversee tool calls."
                    .into(),
                auto_applicable: true,
            });
        }

        if config.allow_list_has_wildcard {
            recs.push(ConfigRecommendation {
                id: "scope-permissions".into(),
                severity: Severity::Critical,
                title: "Permission allow list contains a wildcard".into(),
                description: "The `permissions.allow` list contains `*`, which \
grants every tool unconditionally. Scope it to the specific tools the project \
needs."
                    .into(),
                auto_applicable: false,
            });
        }

        if !config.has_agents {
            recs.push(ConfigRecommendation {
                id: "deploy-agents".into(),
                severity: Severity::Info,
                title: "No agents deployed".into(),
                description: "No agent files were found. Deploy the trusty-mpm \
agents so delegated work runs under managed agents."
                    .into(),
                auto_applicable: false,
            });
        }

        if !config.has_openrouter_key {
            recs.push(ConfigRecommendation {
                id: "add-openrouter-key".into(),
                severity: Severity::Warning,
                title: "OPENROUTER_API_KEY not in env hooks".into(),
                description: "The LLM overseer needs `OPENROUTER_API_KEY`. Add \
it to the Claude Code `env` block (or to `.env.local`)."
                    .into(),
                auto_applicable: false,
            });
        }

        // Order most-severe first so the dashboard lists security issues at the
        // top. `sort_by_key` is stable, so equal-severity recommendations keep
        // their detection order.
        recs.sort_by_key(|r| std::cmp::Reverse(severity_rank(r.severity)));
        recs
    }

    /// Apply a single recommendation, writing the fix to disk.
    ///
    /// Why: lets `POST /claude-config/apply` act on a recommendation without the
    /// operator hand-editing JSON. Every apply is preceded by a checkpoint so
    /// the change is always reversible.
    /// What: first snapshots the project's config via [`ConfigCheckpointer`]
    /// with a `before-{id}` label, then dispatches on `rec.id`. Only
    /// `add-trusty-hooks` is auto-applicable — it writes a minimal `hooks` block
    /// into the project `settings.json`. Recommendations that are not
    /// auto-applicable return an error explaining they need a manual fix.
    /// Returns the checkpoint id so the caller can offer an undo.
    /// Test: `apply_add_hooks_writes_settings`, `apply_manual_rec_errors`,
    /// `apply_creates_checkpoint_before_change`.
    pub fn apply_recommendation(
        rec: &ConfigRecommendation,
        paths: &ClaudeConfigPaths,
        project: &Path,
    ) -> Result<String> {
        // Snapshot first so any failure leaves a restorable checkpoint behind.
        let label = format!("before-{}", rec.id);
        let checkpoint_id = ConfigCheckpointer::create(paths, project, Some(&label))?;
        match rec.id.as_str() {
            "add-trusty-hooks" => {
                apply_add_hooks(&paths.project_settings)?;
                Ok(checkpoint_id)
            }
            other => Err(Error::Protocol(format!(
                "recommendation `{other}` is not auto-applicable; apply it manually"
            ))),
        }
    }
}

/// Rank a [`Severity`] for ordering (higher = more severe).
///
/// Why: `analyze` lists recommendations most-severe first; an explicit numeric
/// rank keeps the ordering independent of the enum's declaration order.
/// What: maps `Info` → 0, `Warning` → 1, `Critical` → 2.
/// Test: `analyze_partial_config_multiple_recs` (Critical sorts before Warning).
pub(super) fn severity_rank(severity: Severity) -> u8 {
    match severity {
        Severity::Info => 0,
        Severity::Warning => 1,
        Severity::Critical => 2,
    }
}

/// Read a JSON file, returning `None` when absent or malformed.
///
/// Why: settings files are optional and operator-edited; a missing or broken
/// file must never abort analysis.
/// What: reads `path`, parses it as JSON; logs and returns `None` on any error.
/// Test: `read_config_missing_files_is_empty` (missing path → `None`).
pub(super) fn read_json(path: &Path) -> Option<Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&raw) {
        Ok(json) => Some(json),
        Err(e) => {
            tracing::warn!("malformed Claude config {}: {e}; skipping", path.display());
            None
        }
    }
}

/// OR-merge one settings JSON document's facts into a [`ClaudeConfig`].
///
/// Why: settings are layered (user → user.local → project → project.local);
/// the analyzer cares only whether *any* layer sets a fact, so booleans are
/// OR-merged and the allow-list count is summed.
/// What: sets `has_hooks` if the doc has a non-empty `hooks` object, scans
/// `permissions.allow` for a `*` and counts its entries, and checks the `env`
/// block for `OPENROUTER_API_KEY`.
/// Test: `read_config_detects_hooks`, `analyze_flags_wildcard`.
fn merge_settings(config: &mut ClaudeConfig, json: &Value) {
    if let Some(hooks) = json.get("hooks").and_then(Value::as_object)
        && !hooks.is_empty()
    {
        config.has_hooks = true;
    }
    if let Some(allow) = json
        .get("permissions")
        .and_then(|p| p.get("allow"))
        .and_then(Value::as_array)
    {
        config.allow_list_entries += allow.len();
        if allow.iter().any(|v| v.as_str() == Some("*")) {
            config.allow_list_has_wildcard = true;
        }
    }
    if let Some(env) = json.get("env").and_then(Value::as_object)
        && env.contains_key("OPENROUTER_API_KEY")
    {
        config.has_openrouter_key = true;
    }
}

/// True when `dir` exists and contains at least one `*.md` agent file.
///
/// Why: an agents directory may exist but be empty; the recommendation cares
/// about actual agent files.
/// What: scans `dir` for a directory entry with a `.md` extension.
/// Test: `read_config_detects_agents`.
fn dir_has_agent_files(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.path()
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| x.eq_ignore_ascii_case("md"))
    })
}

/// Write a minimal trusty-mpm `hooks` block into a project `settings.json`.
///
/// Why: the `add-trusty-hooks` recommendation is auto-applicable; this is its
/// effect.
/// What: reads the existing `settings.json` (or starts from `{}`), inserts a
/// `hooks` object covering `PreToolUse` / `PostToolUse` / `Stop`, creates the
/// `.claude` directory if needed, and writes the file back pretty-printed.
/// Test: `apply_add_hooks_writes_settings`.
fn apply_add_hooks(settings_path: &Path) -> Result<()> {
    let mut json: Value = read_json(settings_path).unwrap_or_else(|| serde_json::json!({}));
    let hooks = serde_json::json!({
        "PreToolUse": [{ "matcher": "*", "hooks": [
            { "type": "command", "command": "trusty-mpm hook" }
        ] }],
        "PostToolUse": [{ "matcher": "*", "hooks": [
            { "type": "command", "command": "trusty-mpm hook" }
        ] }],
        "Stop": [{ "matcher": "*", "hooks": [
            { "type": "command", "command": "trusty-mpm hook" }
        ] }],
    });
    if let Some(obj) = json.as_object_mut() {
        obj.insert("hooks".to_string(), hooks);
    } else {
        json = serde_json::json!({ "hooks": hooks });
    }
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    let pretty = serde_json::to_string_pretty(&json)
        .map_err(|e| Error::Protocol(format!("serialize settings.json: {e}")))?;
    std::fs::write(settings_path, pretty).map_err(Error::Io)?;
    Ok(())
}
