//! Console-facing MCP tool descriptors (#1222 / P2).
//!
//! Why: trusty-console renders the Sessions tab by polling the daemon over MCP
//! (#1104 keeps HTTP only in the console). It needs three tools the original
//! catalog lacked: the service-agnostic `console_metrics` report, a
//! `supervisor_status` fleet snapshot, and `auto_resume_set` — the non-CLI
//! control for enabling/disabling supervisor auto-resume (RFC §6 Q6). Keeping
//! their descriptors here keeps `tools/core.rs` and `tools/session.rs` each well
//! under the 500-SLOC cap.
//! What: [`console_tools`] returns the three `{ name, description, inputSchema }`
//! descriptors in catalog order. The shared [`tool`] builder is re-exported from
//! the parent module.
//! Test: `super::tests::console_tools_present`,
//! `super::tests::catalog_names_match_constant`.

use serde_json::{Value, json};

use super::tool;

/// Build the five console-facing tool descriptors.
///
/// Why: the console poller (`console_metrics`), the Sessions supervisor widget
/// (`supervisor_status`), the auto-resume toggle (`auto_resume_set`), and the
/// #1220 Config tab (`config_read` / `config_write`) each map to one descriptor
/// here; a dedicated builder keeps the catalog modular.
/// What: returns the five descriptors. `console_metrics`, `supervisor_status`, and
/// `config_read` take no arguments; `auto_resume_set` requires a boolean `enabled`;
/// `config_write` takes an optional `workspace_root_template`/`default_model`
/// string and an optional `auto_resume` boolean. All schemas set
/// `additionalProperties: false`.
/// Test: `super::tests::console_tools_present`.
pub(super) fn console_tools() -> Vec<Value> {
    vec![
        tool(
            "console_metrics",
            "Return the standard trusty-console metrics report for trusty-mpm: \
             service id, display name, version, coarse health status, and a \
             `metrics` payload carrying the managed-session fleet snapshot \
             (counts by lifecycle state) and the supervisor auto-resume control \
             state. Polled uniformly by trusty-console for the dashboard.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        tool(
            "supervisor_status",
            "Return the managed-session fleet snapshot and the supervisor \
             auto-resume control state as `{ fleet, auto_resume }`. `fleet` carries \
             counts by lifecycle state (provisioning/active/stopped/errored/\
             decommissioned), pending decisions, and last activity; `auto_resume` \
             carries the persisted desired flag, the supervisor's boot-time env \
             flag, and whether a restart is pending.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        tool(
            "auto_resume_set",
            "Enable or disable supervisor auto-resume by persisting the operator's \
             desired flag to `~/.trusty-mpm/auto_resume`. The 24/7 supervisor reads \
             this on its next sweep; the env var the supervisor booted with stays \
             in force until then (the response's `pending_restart` flags the \
             difference). This is the console's non-CLI auto-resume control.",
            json!({
                "type": "object",
                "properties": {
                    "enabled": {
                        "type": "boolean",
                        "description": "True to enable auto-resume of stopped sessions; false to disable."
                    }
                },
                "required": ["enabled"],
                "additionalProperties": false
            }),
        ),
        tool(
            "config_read",
            "Read trusty-mpm's `~/.trusty-tools/trusty-mpm/config.yaml` (the #1220 \
             cross-crate config convention) and return its current settings as JSON: \
             `workspace_root_template`, `auto_resume`, `default_model`, the global \
             `github` GitHub-identity binding, the global `untracked_sync` \
             untracked/secret-file sync allowlist (#2196), and the full `projects` \
             list (each entry may carry its own `github`/`commit_name`/`commit_email`/ \
             `untracked_sync` per-project override). An absent file returns the \
             defaults (all null/empty). Backs the trusty-console Config tab.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        tool(
            "config_write",
            "Write trusty-mpm's `~/.trusty-tools/trusty-mpm/config.yaml` (#1220, \
             #2184). Supplied fields replace the corresponding settings; omitted \
             fields are left unchanged. `workspace_root_template` sets the \
             managed-session workspace root (a leading `~` is expanded); \
             `auto_resume` sets the supervisor default; `default_model` sets the \
             launch model. The `github_*` fields set a GitHub-CLI identity binding \
             (`config_dir` > `token_env` > `account`, plus `host`) — with no \
             `project_name` they set the GLOBAL binding; with `project_name` set to \
             an ALREADY-REGISTERED project (via `project_register` or a static \
             `config.projects` entry) they set that project's OWN binding instead, \
             which takes precedence over the global one for that project's `gh` \
             calls. `commit_name`/`commit_email` set a per-project git commit-author \
             override applied to managed/provisioner git operations — they REQUIRE \
             `project_name` (commit identity has no global tier). `untracked_sync_patterns`/ \
             `untracked_sync_enabled` (#2196) set the allowlist of untracked/secret filename \
             patterns (default `.env*`) synced from the operator's live checkout into each \
             session worktree at spawn, and the on/off toggle for that sync; like `github_*` \
             (and unlike commit identity) these DO have a global tier — omit `project_name` to \
             set it, or supply an ALREADY-REGISTERED `project_name` to override it for that \
             project only. Returns the merged config that was persisted.",
            json!({
                "type": "object",
                "properties": {
                    "workspace_root_template": {
                        "type": "string",
                        "description": "Template dir for session workspaces (e.g. `~/trusty-mpm-projects`)."
                    },
                    "auto_resume": {
                        "type": "boolean",
                        "description": "Default supervisor auto-resume preference."
                    },
                    "default_model": {
                        "type": "string",
                        "description": "Default model id or tier alias for launched sessions."
                    },
                    "project_name": {
                        "type": "string",
                        "description": "Route the github_*/commit_* fields to this ALREADY-REGISTERED project's own binding instead of the global one."
                    },
                    "github_config_dir": {
                        "type": "string",
                        "description": "Private gh config home (GH_CONFIG_DIR) — highest-precedence identity strategy."
                    },
                    "github_token_env": {
                        "type": "string",
                        "description": "NAME (never the value) of an env var to resolve a gh token from at call time."
                    },
                    "github_account": {
                        "type": "string",
                        "description": "gh username documenting intent; must be paired with github_config_dir to actually select an identity."
                    },
                    "github_host": {
                        "type": "string",
                        "description": "gh host (e.g. a GitHub Enterprise host); defaults to github.com."
                    },
                    "commit_name": {
                        "type": "string",
                        "description": "Git commit author name override for this project (requires project_name)."
                    },
                    "commit_email": {
                        "type": "string",
                        "description": "Git commit author email override for this project (requires project_name)."
                    },
                    "untracked_sync_patterns": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Allowlist of untracked/secret filename patterns to sync into session worktrees (e.g. [\".env\", \".env.local\", \".env.*\"]). Global unless project_name is set."
                    },
                    "untracked_sync_enabled": {
                        "type": "boolean",
                        "description": "Whether untracked/secret file sync runs at all. Global unless project_name is set."
                    }
                },
                "additionalProperties": false
            }),
        ),
    ]
}
