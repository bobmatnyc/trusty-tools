//! Claude Code configuration analyzer (I/O side).
//!
//! Why: `trusty-mpm-core::claude_config` defines the pure data model and path
//! resolution; the daemon owns the filesystem reads, the recommendation logic,
//! and the apply/restart actions. Keeping the I/O here preserves `core`'s
//! purity while still letting trusty-mpm inspect and improve a project's
//! Claude Code setup.
//! What: [`ClaudeConfigAnalyzer`] reads + merges the settings files, produces
//! [`ConfigRecommendation`]s, and applies them; [`ClaudeCodeRestarter`] finds
//! running `claude` processes and restarts Claude Code inside a tmux session.
//! Test: `cargo test -p trusty-mpm-daemon claude_config` covers reading,
//! analysis, and apply against temp directories (no real `~/.claude` touched).

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::core::claude_config::{
    CheckpointPaths, ClaudeConfig, ClaudeConfigPaths, ConfigCheckpoint, ConfigRecommendation,
    DeployTarget, DeploymentProfile, HookConfig, PermissionConfig, Severity,
};
use crate::core::tmux::TmuxTarget;
use crate::core::{Error, Result};

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
fn severity_rank(severity: Severity) -> u8 {
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
fn read_json(path: &Path) -> Option<Value> {
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

// ---- config checkpointing & backup/restore ------------------------------

/// The four config files a checkpoint snapshots, keyed by the relative path
/// stored inside the checkpoint.
///
/// Why: `create` and `restore` must agree on exactly which files form a
/// checkpoint and on the stable relative key each is stored under; deriving the
/// list in one place keeps them consistent.
/// What: returns `(relative-key, absolute-path)` pairs for the user- and
/// project-level `settings.json` / `settings.local.json` files.
fn checkpoint_targets(paths: &ClaudeConfigPaths) -> [(&'static str, &Path); 4] {
    [
        ("user/settings.json", paths.user_settings.as_path()),
        (
            "user/settings.local.json",
            paths.user_local_settings.as_path(),
        ),
        ("project/settings.json", paths.project_settings.as_path()),
        (
            "project/settings.local.json",
            paths.project_local_settings.as_path(),
        ),
    ]
}

/// Snapshots and restores a project's Claude Code config files.
///
/// Why: every mutating config operation must be reversible; a checkpoint
/// captures the full pre-change state so a single restore call undoes it.
/// What: `create` writes a [`ConfigCheckpoint`] JSON file, `list` reads them
/// back newest-first, `restore` rewrites the config files from a checkpoint,
/// and `delete` removes one checkpoint.
/// Test: `apply_creates_checkpoint_before_change`, `restore_reverts_to_pre_apply_state`,
/// `checkpoint_list_newest_first`, `safe_restore_does_not_delete_new_files`.
pub struct ConfigCheckpointer;

impl ConfigCheckpointer {
    /// Snapshot every Claude Code config file for `project`.
    ///
    /// Why: callers need a restorable point-in-time copy of the config before
    /// any change.
    /// What: reads each of the four settings files (absent files are simply not
    /// recorded), writes a [`ConfigCheckpoint`] to
    /// `<project>/.trusty-mpm/checkpoints/<id>.json`, and returns the id. The id
    /// is `checkpoint-{YYYYMMDD}-{HHMMSS}-{4-char-random}` so concurrent
    /// checkpoints in the same second do not collide.
    /// Test: `apply_creates_checkpoint_before_change`.
    pub fn create(
        paths: &ClaudeConfigPaths,
        project: &Path,
        label: Option<&str>,
    ) -> Result<String> {
        let now = chrono::Utc::now();
        let id = format!(
            "checkpoint-{}-{}",
            now.format("%Y%m%d-%H%M%S"),
            random_suffix()
        );

        let mut files = HashMap::new();
        for (key, path) in checkpoint_targets(paths) {
            if let Ok(content) = std::fs::read_to_string(path) {
                files.insert(key.to_string(), content);
            }
        }

        let checkpoint = ConfigCheckpoint {
            id: id.clone(),
            created_at: now.to_rfc3339(),
            project: project.to_path_buf(),
            label: label.map(str::to_string),
            files,
        };

        let dir = CheckpointPaths::dir(project);
        std::fs::create_dir_all(&dir).map_err(Error::Io)?;
        let file = CheckpointPaths::for_id(project, &id);
        let json = serde_json::to_string_pretty(&checkpoint)
            .map_err(|e| Error::Protocol(format!("serialize checkpoint: {e}")))?;
        std::fs::write(&file, json).map_err(Error::Io)?;
        tracing::info!("created config checkpoint {id} for {}", project.display());
        Ok(id)
    }

    /// List every checkpoint for `project`, newest first.
    ///
    /// Why: the dashboard offers a restore picker; newest-first matches what an
    /// operator expects.
    /// What: reads each `*.json` file in the checkpoints directory, skipping any
    /// that fail to parse, and sorts by `created_at` descending. A missing
    /// directory yields an empty list.
    /// Test: `checkpoint_list_newest_first`.
    pub fn list(project: &Path) -> Result<Vec<ConfigCheckpoint>> {
        let dir = CheckpointPaths::dir(project);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => return Ok(Vec::new()),
        };
        let mut checkpoints: Vec<ConfigCheckpoint> = entries
            .flatten()
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| x.eq_ignore_ascii_case("json"))
            })
            .filter_map(|e| {
                let raw = std::fs::read_to_string(e.path()).ok()?;
                match serde_json::from_str::<ConfigCheckpoint>(&raw) {
                    Ok(cp) => Some(cp),
                    Err(err) => {
                        tracing::warn!(
                            "skipping malformed checkpoint {}: {err}",
                            e.path().display()
                        );
                        None
                    }
                }
            })
            .collect();
        // Newest first. `created_at` is RFC3339, which sorts lexically.
        checkpoints.sort_by_key(|c| std::cmp::Reverse(c.created_at.clone()));
        Ok(checkpoints)
    }

    /// Restore `project`'s config files to the state in a checkpoint.
    ///
    /// Why: the undo half of the safety model — re-apply a known-good config.
    /// What: loads the checkpoint, then for every file recorded in it rewrites
    /// the on-disk file (creating parent directories as needed). Files that were
    /// *absent* in the checkpoint are left untouched — this is a safe restore,
    /// so config files created after the checkpoint are never deleted.
    /// Test: `restore_reverts_to_pre_apply_state`, `safe_restore_does_not_delete_new_files`.
    pub fn restore(project: &Path, checkpoint_id: &str) -> Result<()> {
        let file = CheckpointPaths::for_id(project, checkpoint_id);
        let raw = std::fs::read_to_string(&file)
            .map_err(|e| Error::Protocol(format!("checkpoint `{checkpoint_id}` not found: {e}")))?;
        let checkpoint: ConfigCheckpoint = serde_json::from_str(&raw)
            .map_err(|e| Error::Protocol(format!("malformed checkpoint `{checkpoint_id}`: {e}")))?;

        let paths = crate::core::claude_config::ClaudeConfigReader::paths_for_project(project);
        for (key, path) in checkpoint_targets(&paths) {
            // Only files captured in the checkpoint are restored. A file absent
            // from `files` was absent at snapshot time and is left as-is.
            if let Some(content) = checkpoint.files.get(key) {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(Error::Io)?;
                }
                std::fs::write(path, content).map_err(Error::Io)?;
            }
        }
        tracing::info!(
            "restored config checkpoint {checkpoint_id} for {}",
            project.display()
        );
        Ok(())
    }

    /// Delete one checkpoint from `project`.
    ///
    /// Why: checkpoints accumulate; the operator needs to prune them.
    /// What: removes `<project>/.trusty-mpm/checkpoints/<id>.json`. A missing
    /// file is reported as a protocol error.
    /// Test: `checkpoint_delete_removes_file`.
    pub fn delete(project: &Path, checkpoint_id: &str) -> Result<()> {
        let file = CheckpointPaths::for_id(project, checkpoint_id);
        std::fs::remove_file(&file).map_err(|e| {
            Error::Protocol(format!("cannot delete checkpoint `{checkpoint_id}`: {e}"))
        })
    }
}

/// A short pseudo-random suffix for checkpoint ids.
///
/// Why: two checkpoints created in the same second must not collide; a UUID's
/// first four hex chars are random enough without pulling in an RNG crate.
/// What: returns the first four characters of a fresh v4 UUID.
/// Test: covered indirectly by `apply_creates_checkpoint_before_change`.
fn random_suffix() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..4].to_string()
}

// ---- deployment profiles ------------------------------------------------

/// Builds and deploys named [`DeploymentProfile`]s onto Claude Code settings.
///
/// Why: operators want one-click configuration presets rather than
/// hand-editing JSON; the deployer turns a profile into concrete settings
/// edits, always behind a checkpoint.
/// What: `builtin_profiles` returns the shipped presets, `deploy` writes a
/// profile (after checkpointing) and returns the checkpoint id, and
/// `list_applied` reports which profile names are detectable in the config.
/// Test: `deploy_trusty_oversight_profile_writes_hooks`,
/// `deploy_readonly_profile_writes_deny_list`.
pub struct ProfileDeployer;

impl ProfileDeployer {
    /// The deployment profiles shipped with trusty-mpm.
    ///
    /// Why: every install offers the same baseline presets — full oversight, a
    /// read-only review mode, and a clean slate.
    /// What: returns `trusty-mpm-oversight`, `read-only-review`, and `minimal`.
    /// Test: `builtin_profiles_are_present`.
    pub fn builtin_profiles() -> Vec<DeploymentProfile> {
        vec![
            DeploymentProfile {
                name: "trusty-mpm-oversight".into(),
                description: "Full oversight: PreToolUse/PostToolUse hooks POST \
to the trusty-mpm daemon, standard dev tools allowed."
                    .into(),
                target: DeployTarget::Project,
                hooks: Some(HookConfig {
                    pre_tool_use: vec![OVERSIGHT_PRE_HOOK.to_string()],
                    post_tool_use: vec![OVERSIGHT_POST_HOOK.to_string()],
                    stop: vec![],
                }),
                permissions: Some(PermissionConfig {
                    allow: vec![
                        "Read".into(),
                        "Glob".into(),
                        "Grep".into(),
                        "Edit".into(),
                        "Write".into(),
                        "Bash".into(),
                    ],
                    deny: vec![],
                }),
                env_vars: HashMap::new(),
            },
            DeploymentProfile {
                name: "read-only-review".into(),
                description: "Code review mode: only Read/Glob/Grep allowed; \
Bash/Write/Edit are denied."
                    .into(),
                target: DeployTarget::Project,
                hooks: None,
                permissions: Some(PermissionConfig {
                    allow: vec!["Read".into(), "Glob".into(), "Grep".into()],
                    deny: vec!["Bash".into(), "Write".into(), "Edit".into()],
                }),
                env_vars: HashMap::new(),
            },
            DeploymentProfile {
                name: "minimal".into(),
                description: "Clean slate: no hooks, permissive allow list.".into(),
                target: DeployTarget::Project,
                hooks: None,
                permissions: Some(PermissionConfig {
                    allow: vec!["Read".into(), "Glob".into(), "Grep".into()],
                    deny: vec![],
                }),
                env_vars: HashMap::new(),
            },
        ]
    }

    /// Deploy a profile onto a project's Claude Code settings.
    ///
    /// Why: applies a preset's hooks, permissions, and env vars in one step,
    /// behind a checkpoint so it is reversible.
    /// What: checkpoints the config (`before-deploy-{name}`), then writes the
    /// profile's values into the settings file(s) selected by its
    /// [`DeployTarget`]. Returns the checkpoint id.
    /// Test: `deploy_trusty_oversight_profile_writes_hooks`,
    /// `deploy_readonly_profile_writes_deny_list`.
    pub fn deploy(
        profile: &DeploymentProfile,
        paths: &ClaudeConfigPaths,
        project: &Path,
    ) -> Result<String> {
        let label = format!("before-deploy-{}", profile.name);
        let checkpoint_id = ConfigCheckpointer::create(paths, project, Some(&label))?;

        let mut targets: Vec<&Path> = Vec::new();
        match profile.target {
            DeployTarget::User => targets.push(&paths.user_settings),
            DeployTarget::Project => targets.push(&paths.project_settings),
            DeployTarget::Both => {
                targets.push(&paths.user_settings);
                targets.push(&paths.project_settings);
            }
        }
        for settings_path in targets {
            write_profile_to_settings(profile, settings_path)?;
        }
        Ok(checkpoint_id)
    }

    /// Report which built-in profile names are detectable in the config.
    ///
    /// Why: the dashboard shows which presets are currently in force.
    /// What: reads the merged config and matches it heuristically against each
    /// built-in profile — a profile counts as applied when its non-empty deny
    /// list and hook commands are all present in the settings.
    /// Test: `list_applied_detects_deployed_profile`.
    pub fn list_applied(paths: &ClaudeConfigPaths) -> Result<Vec<String>> {
        let mut merged: Vec<Value> = Vec::new();
        for path in [
            &paths.user_settings,
            &paths.user_local_settings,
            &paths.project_settings,
            &paths.project_local_settings,
        ] {
            if let Some(json) = read_json(path) {
                merged.push(json);
            }
        }
        let applied = Self::builtin_profiles()
            .into_iter()
            .filter(|p| profile_is_applied(p, &merged))
            .map(|p| p.name)
            .collect();
        Ok(applied)
    }
}

/// The `PreToolUse` hook command the `trusty-mpm-oversight` profile installs.
const OVERSIGHT_PRE_HOOK: &str = "curl -s -X POST http://localhost:7373/hooks -H 'Content-Type: application/json' -d '{\"session_id\":\"${CLAUDE_SESSION_ID}\",\"event\":\"PreToolUse\",\"payload\":{\"tool\":\"${CLAUDE_TOOL_NAME}\",\"input\":${CLAUDE_TOOL_INPUT}}}' || true";

/// The `PostToolUse` hook command the `trusty-mpm-oversight` profile installs.
const OVERSIGHT_POST_HOOK: &str = "curl -s -X POST http://localhost:7373/hooks -H 'Content-Type: application/json' -d '{\"session_id\":\"${CLAUDE_SESSION_ID}\",\"event\":\"PostToolUse\",\"payload\":{\"tool\":\"${CLAUDE_TOOL_NAME}\",\"output\":\"done\"}}' || true";

/// True when `profile`'s distinctive marks are all present in the merged config.
///
/// Why: `list_applied` needs a deterministic "is this preset in force?" check
/// without storing extra state in the settings files.
/// What: a profile counts as applied when every deny-list entry it defines and
/// every hook command it installs appears somewhere in the merged settings
/// documents. Profiles with neither a deny list nor hooks (e.g. `minimal`) are
/// never reported, as they leave no detectable footprint.
fn profile_is_applied(profile: &DeploymentProfile, merged: &[Value]) -> bool {
    let deny: Vec<&str> = profile
        .permissions
        .as_ref()
        .map(|p| p.deny.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let hook_cmds: Vec<&str> = profile
        .hooks
        .as_ref()
        .map(|h| {
            h.pre_tool_use
                .iter()
                .chain(&h.post_tool_use)
                .chain(&h.stop)
                .map(String::as_str)
                .collect()
        })
        .unwrap_or_default();
    if deny.is_empty() && hook_cmds.is_empty() {
        return false;
    }
    let blob = merged
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    deny.iter().all(|d| {
        // Match the deny entry as a JSON string token to avoid substring noise.
        blob.contains(&format!("\"{d}\""))
    }) && hook_cmds.iter().all(|c| blob.contains(c))
}

/// Write a deployment profile's values into one `settings.json`.
///
/// Why: `deploy` may touch one or two settings files; the per-file edit logic
/// belongs in one helper.
/// What: reads the existing settings (or `{}`), inserts a `hooks` block when the
/// profile defines hooks, a `permissions` block when it defines permissions, and
/// merges its `env_vars` into the `env` block. Creates the `.claude` directory
/// if needed and writes the file back pretty-printed.
/// Test: `deploy_trusty_oversight_profile_writes_hooks`.
fn write_profile_to_settings(profile: &DeploymentProfile, settings_path: &Path) -> Result<()> {
    let mut json: Value = read_json(settings_path).unwrap_or_else(|| serde_json::json!({}));
    let obj = match json.as_object_mut() {
        Some(obj) => obj,
        None => {
            json = serde_json::json!({});
            json.as_object_mut().expect("freshly built object")
        }
    };

    if let Some(hooks) = &profile.hooks {
        obj.insert("hooks".to_string(), hook_config_to_json(hooks));
    }
    if let Some(perms) = &profile.permissions {
        obj.insert(
            "permissions".to_string(),
            serde_json::json!({ "allow": perms.allow, "deny": perms.deny }),
        );
    }
    if !profile.env_vars.is_empty() {
        let env = obj
            .entry("env".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if let Some(env_obj) = env.as_object_mut() {
            for (k, v) in &profile.env_vars {
                env_obj.insert(k.clone(), Value::String(v.clone()));
            }
        }
    }

    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    let pretty = serde_json::to_string_pretty(&json)
        .map_err(|e| Error::Protocol(format!("serialize settings.json: {e}")))?;
    std::fs::write(settings_path, pretty).map_err(Error::Io)?;
    Ok(())
}

/// Render a [`HookConfig`] as a Claude Code `hooks` JSON object.
///
/// Why: Claude Code expects hooks grouped by event with `matcher`/`hooks`
/// entries; the profile model stores plain command strings, so a translation
/// step is needed.
/// What: builds a `{ "PreToolUse": [...], ... }` object, emitting an event key
/// only when the profile defines at least one command for it.
/// Test: `deploy_trusty_oversight_profile_writes_hooks`.
fn hook_config_to_json(hooks: &HookConfig) -> Value {
    let mut obj = serde_json::Map::new();
    for (event, commands) in [
        ("PreToolUse", &hooks.pre_tool_use),
        ("PostToolUse", &hooks.post_tool_use),
        ("Stop", &hooks.stop),
    ] {
        if commands.is_empty() {
            continue;
        }
        let entries: Vec<Value> = commands
            .iter()
            .map(|cmd| {
                serde_json::json!({
                    "matcher": "",
                    "hooks": [{ "type": "command", "command": cmd }],
                })
            })
            .collect();
        obj.insert(event.to_string(), Value::Array(entries));
    }
    Value::Object(obj)
}

/// Finds and restarts running Claude Code processes.
///
/// Why: after applying config changes the operator wants Claude Code to pick
/// them up; this drives the restart.
/// What: `find_claude_processes` lists `claude` PIDs via `pgrep`;
/// `restart_in_session` sends Ctrl-C then `claude` into a tmux session's pane.
/// Test: `find_claude_processes_does_not_panic` (the PID list may be empty).
pub struct ClaudeCodeRestarter;

impl ClaudeCodeRestarter {
    /// List the PIDs of running `claude` processes.
    ///
    /// Why: the dashboard shows whether Claude Code is running and how many
    /// instances; the restart flow can also use it to confirm a target exists.
    /// What: runs `pgrep -x claude`; a non-zero exit (no matches) or a missing
    /// `pgrep` both yield an empty `Vec` rather than an error.
    /// Test: `find_claude_processes_does_not_panic`.
    pub fn find_claude_processes() -> Vec<u32> {
        let output = match Command::new("pgrep").args(["-x", "claude"]).output() {
            Ok(out) => out,
            Err(e) => {
                tracing::info!("pgrep unavailable: {e}; reporting no claude processes");
                return Vec::new();
            }
        };
        if !output.status.success() {
            return Vec::new();
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|l| l.trim().parse::<u32>().ok())
            .collect()
    }

    /// Restart Claude Code inside a named tmux session.
    ///
    /// Why: a Claude Code session hosted in tmux is restarted in place — send
    /// an interrupt to stop the current process, then relaunch `claude`.
    /// What: discovers tmux, sends `C-c` to the session's pane, waits briefly
    /// for the process to exit, then types `claude` + Enter. tmux being absent
    /// surfaces as an `Err`.
    /// Test: `restart_in_session_errors_without_tmux` (skipped when tmux is
    /// installed).
    pub fn restart_in_session(tmux_session: &str) -> Result<()> {
        let driver = super::tmux::TmuxDriver::discover()?;
        let target = TmuxTarget::session(tmux_session);
        // Interrupt the running Claude Code process.
        driver.send_interrupt(&target)?;
        std::thread::sleep(std::time::Duration::from_millis(500));
        // Relaunch Claude Code.
        driver.send_line(&target, "claude")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::claude_config::ClaudeConfigReader;

    /// Build a `ClaudeConfigPaths` rooted entirely under a temp directory so a
    /// test never reads or writes the operator's real `~/.claude`.
    fn temp_paths(root: &Path) -> ClaudeConfigPaths {
        let project = root.join("project");
        let user = root.join("home");
        ClaudeConfigPaths {
            user_settings: user.join(".claude/settings.json"),
            user_local_settings: user.join(".claude/settings.local.json"),
            project_settings: project.join(".claude/settings.json"),
            project_local_settings: project.join(".claude/settings.local.json"),
            user_agents_dir: user.join(".claude/agents"),
            project_agents_dir: project.join(".claude/agents"),
        }
    }

    fn write_json(path: &Path, json: &Value) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_string_pretty(json).unwrap()).unwrap();
    }

    #[test]
    fn read_config_missing_files_is_empty() {
        // No settings files on disk → an all-default ClaudeConfig.
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        let config = ClaudeConfigAnalyzer::read_config(&paths);
        assert_eq!(config, ClaudeConfig::default());
    }

    #[test]
    fn read_config_detects_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        write_json(
            &paths.project_settings,
            &serde_json::json!({ "hooks": { "PreToolUse": [] } }),
        );
        let config = ClaudeConfigAnalyzer::read_config(&paths);
        assert!(config.has_hooks);
    }

    #[test]
    fn read_config_detects_wildcard_and_env() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        write_json(
            &paths.project_settings,
            &serde_json::json!({
                "permissions": { "allow": ["*", "Read"] },
                "env": { "OPENROUTER_API_KEY": "sk-x" } // pragma: allowlist secret
            }),
        );
        let config = ClaudeConfigAnalyzer::read_config(&paths);
        assert!(config.allow_list_has_wildcard);
        assert_eq!(config.allow_list_entries, 2);
        assert!(config.has_openrouter_key);
    }

    #[test]
    fn read_config_detects_agents() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        std::fs::create_dir_all(&paths.project_agents_dir).unwrap();
        std::fs::write(paths.project_agents_dir.join("research.md"), "# agent").unwrap();
        let config = ClaudeConfigAnalyzer::read_config(&paths);
        assert!(config.has_agents);
    }

    #[test]
    fn analyze_flags_missing_hooks() {
        // A default (empty) config triggers the add-trusty-hooks recommendation.
        let recs = ClaudeConfigAnalyzer::analyze(&ClaudeConfig::default());
        assert!(recs.iter().any(|r| r.id == "add-trusty-hooks"));
    }

    #[test]
    fn analyze_flags_wildcard() {
        let config = ClaudeConfig {
            has_hooks: true,
            allow_list_has_wildcard: true,
            allow_list_entries: 1,
            has_agents: true,
            has_openrouter_key: true,
        };
        let recs = ClaudeConfigAnalyzer::analyze(&config);
        let wildcard = recs.iter().find(|r| r.id == "scope-permissions");
        assert!(wildcard.is_some());
        assert_eq!(wildcard.unwrap().severity, Severity::Critical);
    }

    #[test]
    fn analyze_clean_config_is_empty() {
        // A fully-configured project yields no recommendations.
        let config = ClaudeConfig {
            has_hooks: true,
            allow_list_has_wildcard: false,
            allow_list_entries: 5,
            has_agents: true,
            has_openrouter_key: true,
        };
        assert!(ClaudeConfigAnalyzer::analyze(&config).is_empty());
    }

    #[test]
    fn apply_add_hooks_writes_settings() {
        // Applying add-trusty-hooks must write a hooks block that a subsequent
        // read picks up as `has_hooks = true`.
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        let project = dir.path().join("project");
        let rec = ClaudeConfigAnalyzer::analyze(&ClaudeConfig::default())
            .into_iter()
            .find(|r| r.id == "add-trusty-hooks")
            .expect("add-trusty-hooks recommended");
        ClaudeConfigAnalyzer::apply_recommendation(&rec, &paths, &project).expect("apply succeeds");

        let config = ClaudeConfigAnalyzer::read_config(&paths);
        assert!(config.has_hooks, "hooks block must be present after apply");
    }

    #[test]
    fn apply_manual_rec_errors() {
        // A non-auto-applicable recommendation cannot be applied programmatically.
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        let project = dir.path().join("project");
        let rec = ConfigRecommendation {
            id: "scope-permissions".into(),
            severity: Severity::Critical,
            title: "x".into(),
            description: "x".into(),
            auto_applicable: false,
        };
        assert!(ClaudeConfigAnalyzer::apply_recommendation(&rec, &paths, &project).is_err());
    }

    // ---- deterministic analysis coverage --------------------------------

    /// A fully-configured `ClaudeConfig` — the baseline a test mutates to
    /// trigger exactly one recommendation.
    fn healthy_config() -> ClaudeConfig {
        ClaudeConfig {
            has_hooks: true,
            allow_list_has_wildcard: false,
            allow_list_entries: 5,
            has_agents: true,
            has_openrouter_key: true,
        }
    }

    #[test]
    fn analyze_missing_hooks_flags_warning() {
        // No hooks → exactly the add-trusty-hooks rec, at Warning severity.
        let config = ClaudeConfig {
            has_hooks: false,
            ..healthy_config()
        };
        let recs = ClaudeConfigAnalyzer::analyze(&config);
        let rec = recs
            .iter()
            .find(|r| r.id == "add-trusty-hooks")
            .expect("add-trusty-hooks flagged");
        assert_eq!(rec.severity, Severity::Warning);
        assert_eq!(recs.len(), 1, "only the missing-hooks issue");
    }

    #[test]
    fn analyze_wildcard_permission_flags_critical() {
        // A `*` in the allow list → scope-permissions at Critical severity.
        let config = ClaudeConfig {
            allow_list_has_wildcard: true,
            ..healthy_config()
        };
        let recs = ClaudeConfigAnalyzer::analyze(&config);
        let rec = recs
            .iter()
            .find(|r| r.id == "scope-permissions")
            .expect("scope-permissions flagged");
        assert_eq!(rec.severity, Severity::Critical);
        assert_eq!(recs.len(), 1, "only the wildcard issue");
    }

    #[test]
    fn analyze_no_agents_flags_info() {
        // No agent files → deploy-agents at Info severity.
        let config = ClaudeConfig {
            has_agents: false,
            ..healthy_config()
        };
        let recs = ClaudeConfigAnalyzer::analyze(&config);
        let rec = recs
            .iter()
            .find(|r| r.id == "deploy-agents")
            .expect("deploy-agents flagged");
        assert_eq!(rec.severity, Severity::Info);
        assert_eq!(recs.len(), 1, "only the missing-agents issue");
    }

    #[test]
    fn analyze_missing_openrouter_key_flags_warning() {
        // No OPENROUTER_API_KEY → add-openrouter-key at Warning severity.
        let config = ClaudeConfig {
            has_openrouter_key: false,
            ..healthy_config()
        };
        let recs = ClaudeConfigAnalyzer::analyze(&config);
        let rec = recs
            .iter()
            .find(|r| r.id == "add-openrouter-key")
            .expect("add-openrouter-key flagged");
        assert_eq!(rec.severity, Severity::Warning);
        assert_eq!(recs.len(), 1, "only the missing-key issue");
    }

    #[test]
    fn analyze_fully_configured_is_empty() {
        // Hooks + scoped perms + agents + key → zero recommendations.
        assert!(ClaudeConfigAnalyzer::analyze(&healthy_config()).is_empty());
    }

    #[test]
    fn analyze_partial_config_multiple_recs() {
        // Missing hooks AND a wildcard → two recommendations, Critical first.
        let config = ClaudeConfig {
            has_hooks: false,
            allow_list_has_wildcard: true,
            ..healthy_config()
        };
        let recs = ClaudeConfigAnalyzer::analyze(&config);
        assert_eq!(recs.len(), 2, "exactly the two flagged issues");
        assert_eq!(
            recs[0].severity,
            Severity::Critical,
            "Critical sorts before Warning"
        );
        assert_eq!(recs[0].id, "scope-permissions");
        assert_eq!(recs[1].severity, Severity::Warning);
        assert_eq!(recs[1].id, "add-trusty-hooks");
    }

    // ---- checkpointing & backup/restore ---------------------------------

    #[test]
    fn apply_creates_checkpoint_before_change() {
        // Applying a recommendation must leave a checkpoint file behind in
        // `.trusty-mpm/checkpoints/`.
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        let project = dir.path().join("project");
        let rec = ClaudeConfigAnalyzer::analyze(&ClaudeConfig::default())
            .into_iter()
            .find(|r| r.id == "add-trusty-hooks")
            .expect("add-trusty-hooks recommended");
        let checkpoint_id = ClaudeConfigAnalyzer::apply_recommendation(&rec, &paths, &project)
            .expect("apply succeeds");

        let cp_file = crate::core::claude_config::CheckpointPaths::for_id(&project, &checkpoint_id);
        assert!(cp_file.exists(), "checkpoint JSON must exist after apply");
        let checkpoints = ConfigCheckpointer::list(&project).unwrap();
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints[0].id, checkpoint_id);
        assert_eq!(
            checkpoints[0].label.as_deref(),
            Some("before-add-trusty-hooks")
        );
    }

    #[test]
    fn restore_reverts_to_pre_apply_state() {
        // Apply a change, then restore the pre-apply checkpoint: the project
        // settings.json must return to its original (absent) state-equivalent.
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        let project = dir.path().join("project");

        // Start from a project settings.json with no hooks.
        write_json(&paths.project_settings, &serde_json::json!({ "x": 1 }));

        let rec = ConfigRecommendation {
            id: "add-trusty-hooks".into(),
            severity: Severity::Warning,
            title: "x".into(),
            description: "x".into(),
            auto_applicable: true,
        };
        let checkpoint_id = ClaudeConfigAnalyzer::apply_recommendation(&rec, &paths, &project)
            .expect("apply succeeds");
        assert!(
            ClaudeConfigAnalyzer::read_config(&paths).has_hooks,
            "hooks present after apply"
        );

        ConfigCheckpointer::restore(&project, &checkpoint_id).expect("restore succeeds");
        let restored: Value =
            serde_json::from_str(&std::fs::read_to_string(&paths.project_settings).unwrap())
                .unwrap();
        assert_eq!(restored, serde_json::json!({ "x": 1 }), "original content");
        assert!(
            !ClaudeConfigAnalyzer::read_config(&paths).has_hooks,
            "hooks gone after restore"
        );
    }

    #[test]
    fn checkpoint_list_newest_first() {
        // Three checkpoints created in sequence list newest-first.
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        let project = dir.path().join("project");

        let mut ids = Vec::new();
        for label in ["first", "second", "third"] {
            let id = ConfigCheckpointer::create(&paths, &project, Some(label)).unwrap();
            ids.push(id);
            // Sleep past the 1-second timestamp resolution so ordering is
            // deterministic regardless of the random suffix.
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }

        let listed = ConfigCheckpointer::list(&project).unwrap();
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].id, ids[2], "newest first");
        assert_eq!(listed[1].id, ids[1]);
        assert_eq!(listed[2].id, ids[0], "oldest last");
    }

    #[test]
    fn safe_restore_does_not_delete_new_files() {
        // A config file created AFTER the checkpoint must survive a restore —
        // restore only rewrites files that were captured in the checkpoint.
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        let project = dir.path().join("project");

        // Checkpoint with only the project settings.json present.
        write_json(&paths.project_settings, &serde_json::json!({ "a": 1 }));
        let checkpoint_id = ConfigCheckpointer::create(&paths, &project, Some("snapshot")).unwrap();

        // After the checkpoint, a new local-settings file appears.
        write_json(
            &paths.project_local_settings,
            &serde_json::json!({ "new": true }),
        );

        ConfigCheckpointer::restore(&project, &checkpoint_id).expect("restore succeeds");
        assert!(
            paths.project_local_settings.exists(),
            "file created after the checkpoint must not be deleted by restore"
        );
        let still: Value =
            serde_json::from_str(&std::fs::read_to_string(&paths.project_local_settings).unwrap())
                .unwrap();
        assert_eq!(still, serde_json::json!({ "new": true }));
    }

    #[test]
    fn checkpoint_delete_removes_file() {
        // Deleting a checkpoint removes its JSON file.
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        let project = dir.path().join("project");
        let id = ConfigCheckpointer::create(&paths, &project, None).unwrap();
        assert_eq!(ConfigCheckpointer::list(&project).unwrap().len(), 1);
        ConfigCheckpointer::delete(&project, &id).expect("delete succeeds");
        assert!(ConfigCheckpointer::list(&project).unwrap().is_empty());
    }

    // ---- deployment profiles --------------------------------------------

    #[test]
    fn builtin_profiles_are_present() {
        let names: Vec<String> = ProfileDeployer::builtin_profiles()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert!(names.contains(&"trusty-mpm-oversight".to_string()));
        assert!(names.contains(&"read-only-review".to_string()));
        assert!(names.contains(&"minimal".to_string()));
    }

    #[test]
    fn deploy_trusty_oversight_profile_writes_hooks() {
        // Deploying the oversight profile must write a hooks block into the
        // project settings.json.
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        let project = dir.path().join("project");
        let profile = ProfileDeployer::builtin_profiles()
            .into_iter()
            .find(|p| p.name == "trusty-mpm-oversight")
            .expect("oversight profile exists");
        ProfileDeployer::deploy(&profile, &paths, &project).expect("deploy succeeds");

        let settings: Value =
            serde_json::from_str(&std::fs::read_to_string(&paths.project_settings).unwrap())
                .unwrap();
        assert!(
            settings["hooks"]["PreToolUse"].is_array(),
            "PreToolUse hooks written"
        );
        assert!(
            settings["hooks"]["PostToolUse"].is_array(),
            "PostToolUse hooks written"
        );
        assert!(
            ClaudeConfigAnalyzer::read_config(&paths).has_hooks,
            "deployed hooks are detected by the analyzer"
        );
    }

    #[test]
    fn deploy_readonly_profile_writes_deny_list() {
        // Deploying the read-only profile must write a permissions deny list.
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        let project = dir.path().join("project");
        let profile = ProfileDeployer::builtin_profiles()
            .into_iter()
            .find(|p| p.name == "read-only-review")
            .expect("read-only profile exists");
        ProfileDeployer::deploy(&profile, &paths, &project).expect("deploy succeeds");

        let settings: Value =
            serde_json::from_str(&std::fs::read_to_string(&paths.project_settings).unwrap())
                .unwrap();
        let deny = settings["permissions"]["deny"]
            .as_array()
            .expect("deny list present");
        assert!(deny.iter().any(|v| v == "Bash"));
        assert!(deny.iter().any(|v| v == "Write"));
        assert!(deny.iter().any(|v| v == "Edit"));
    }

    #[test]
    fn list_applied_detects_deployed_profile() {
        // After deploying the read-only profile, list_applied reports it.
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        let project = dir.path().join("project");
        let profile = ProfileDeployer::builtin_profiles()
            .into_iter()
            .find(|p| p.name == "read-only-review")
            .expect("read-only profile exists");
        ProfileDeployer::deploy(&profile, &paths, &project).expect("deploy succeeds");

        let applied = ProfileDeployer::list_applied(&paths).unwrap();
        assert!(applied.contains(&"read-only-review".to_string()));
    }

    #[test]
    fn deploy_creates_checkpoint() {
        // deploy must checkpoint before writing, returning the checkpoint id.
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        let project = dir.path().join("project");
        let profile = ProfileDeployer::builtin_profiles()
            .into_iter()
            .find(|p| p.name == "minimal")
            .expect("minimal profile exists");
        let checkpoint_id =
            ProfileDeployer::deploy(&profile, &paths, &project).expect("deploy succeeds");
        let listed = ConfigCheckpointer::list(&project).unwrap();
        assert!(listed.iter().any(|c| c.id == checkpoint_id));
    }

    #[test]
    fn find_claude_processes_does_not_panic() {
        // Whether or not Claude Code is running, this returns a Vec without
        // panicking — the count is environment-dependent.
        let _pids = ClaudeCodeRestarter::find_claude_processes();
    }

    #[test]
    fn paths_for_project_is_usable() {
        // The core resolver and the analyzer agree on the path shape.
        let paths = ClaudeConfigReader::paths_for_project(Path::new("/work/demo"));
        assert!(paths.project_settings.ends_with(".claude/settings.json"));
    }
}
