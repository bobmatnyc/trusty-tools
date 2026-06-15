//! Session-lifecycle MCP tool descriptors (#1221).
//!
//! Why: the claude-mpm driver skill (#842) previously scraped `tm session` CLI
//! text output, which broke when the documented `--json` flag did not exist.
//! Exposing the managed-session lifecycle as typed MCP tools gives the driver a
//! schema-validated, JSON-native surface — and matches every other trusty-*
//! tool, which all speak MCP. These six tools are thin wrappers over the
//! existing [`crate::session_manager::SessionManager`] lifecycle ops that the
//! HTTP `…/managed/*` routes already use, so the behaviour is identical across
//! transports.
//! What: [`session_tools`] returns the six `{ name, description, inputSchema }`
//! descriptors — `session_new`, `session_stop`, `session_resume`,
//! `session_decommission`, `session_activity`, `session_send`. The shared
//! [`tool`] builder is re-exported from the parent module.
//! Test: `cargo test -p trusty-mpm` (in [`super`]) asserts the full catalog is
//! well-formed and that each of these six names is present.

use serde_json::{Value, json};

use super::tool;

/// Build the six session-lifecycle tool descriptors.
///
/// Why: spawning, stopping, resuming, decommissioning, observing, and driving a
/// managed session are the operations the driver skill needs; surfacing them as
/// MCP tools removes the CLI-scraping defect. Keeping them in their own builder
/// keeps `tools/core.rs` and this file each well under the 500-SLOC cap.
/// What: returns the six descriptors in catalog order. `session_new` takes the
/// repo/ref/task spawn inputs; the other five take a `session_id` (the managed
/// UUID) plus operation-specific fields. Every schema sets
/// `additionalProperties: false` so the driver gets a clear error on a typo.
/// Test: `super::tests::session_tools_present`,
/// `super::tests::catalog_names_match_constant`.
pub(super) fn session_tools() -> Vec<Value> {
    vec![
        tool(
            "session_new",
            "Spawn a new managed Claude Code (or trusty-code) session in an \
             isolated, freshly-provisioned workspace cloned from `repo_url` at \
             `ref`. The daemon creates the tmux host, deploys agents/skills, and \
             launches the harness with the given `task`. Returns the new managed \
             session id, tmux name, workspace path, lifecycle state, and the \
             `tmux attach-session` command.",
            json!({
                "type": "object",
                "properties": {
                    "repo_url": {
                        "type": "string",
                        "description": "Repository URL to clone into the session workspace."
                    },
                    "ref": {
                        "type": "string",
                        "description": "Git branch or ref to check out."
                    },
                    "task": {
                        "type": "string",
                        "description": "Human-readable task description handed to the harness."
                    },
                    "name_hint": {
                        "type": "string",
                        "description": "Optional name hint overriding the auto-generated tmux session name."
                    },
                    "runtime": {
                        "type": "string",
                        "enum": ["claude-code", "tcode"],
                        "description": "Optional runtime backend; defaults to claude-code."
                    }
                },
                "required": ["repo_url", "ref", "task"],
                "additionalProperties": false
            }),
        ),
        tool(
            "session_stop",
            "Stop a managed session's runtime (kills the tmux session and harness \
             process) while PRESERVING its workspace on disk and its record, so it \
             can be resumed later with `session_resume`. This is NOT a teardown — \
             use `session_decommission` to remove the workspace permanently.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "Managed session id (UUID)."
                    }
                },
                "required": ["session_id"],
                "additionalProperties": false
            }),
        ),
        tool(
            "session_resume",
            "Resume a previously-stopped managed session: re-create the tmux host \
             rooted at the still-on-disk workspace and re-spawn the SAME runtime \
             backend the session was created with (no re-clone). Returns the \
             updated session record.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "Managed session id (UUID)."
                    }
                },
                "required": ["session_id"],
                "additionalProperties": false
            }),
        ),
        tool(
            "session_decommission",
            "Permanently tear down a managed session: kill the runtime, REMOVE the \
             workspace directory from disk, and mark the record Decommissioned. \
             This is terminal — the session can NOT be resumed afterwards. A \
             tombstone record is retained for audit.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "Managed session id (UUID)."
                    }
                },
                "required": ["session_id"],
                "additionalProperties": false
            }),
        ),
        tool(
            "session_activity",
            "Inspect a managed session's recent activity. ALWAYS returns the raw \
             tmux pane content (last `lines` lines, default 60) plus structured \
             lifecycle fields (`runtime_active`, `pending_decision`, \
             `proposed_default`) so the caller can do its own inference WITHOUT an \
             LLM key. When OPENROUTER_API_KEY is configured the daemon also \
             returns an LLM `classification` of the session state.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "Managed session id (UUID)."
                    },
                    "lines": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 500,
                        "description": "Number of trailing pane lines to capture (default 60)."
                    }
                },
                "required": ["session_id"],
                "additionalProperties": false
            }),
        ),
        tool(
            "session_send",
            "Send a line of text into a managed session's tmux pane (followed by \
             Enter), e.g. to answer a prompt or drive the harness. Returns a \
             confirmation with the target tmux session name.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "Managed session id (UUID)."
                    },
                    "text": {
                        "type": "string",
                        "description": "Text to inject into the session's pane."
                    }
                },
                "required": ["session_id", "text"],
                "additionalProperties": false
            }),
        ),
    ]
}
