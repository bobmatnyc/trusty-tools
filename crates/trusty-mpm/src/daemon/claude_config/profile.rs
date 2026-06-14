//! [`ProfileDeployer`] — built-in deployment profiles for Claude Code.
//!
//! Why: operators want one-click configuration presets rather than
//! hand-editing JSON; the deployer turns a profile into concrete settings
//! edits, always behind a checkpoint.
//! What: `builtin_profiles` returns the shipped presets, `deploy` writes a
//! profile (after checkpointing) and returns the checkpoint id, and
//! `list_applied` reports which profile names are detectable in the config.
//! Test: see `super::tests`.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use crate::core::claude_config::{
    ClaudeConfigPaths, DeployTarget, DeploymentProfile, HookConfig, PermissionConfig,
};
use crate::core::{Error, Result};

use super::analyzer::read_json;
use super::checkpointer::ConfigCheckpointer;

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
