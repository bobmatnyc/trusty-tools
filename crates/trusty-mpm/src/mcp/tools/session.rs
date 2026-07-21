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
//! What: [`session_tools`] returns the eleven `{ name, description, inputSchema }`
//! descriptors — `session_new`, `session_stop`, `session_resume`,
//! `session_decommission`, `session_delete` (#2012), `session_activity`,
//! `session_send`, the #1508 fleet-wide `session_decommission_ephemeral` +
//! `session_prune`, and the PM pause/resume context tools
//! `session_context_catchup` + `session_context_pause` (replace the bash
//! `tm session catchup` / git / snapshot-file plumbing the `/tm-session-pause`
//! and `/tm-session-resume` skills used to shell out to). The shared [`tool`]
//! builder is re-exported from the parent module.
//! Test: `cargo test -p trusty-mpm` (in [`super`]) asserts the full catalog is
//! well-formed and that each of these names is present.

use serde_json::{Value, json};

use super::tool;

/// Build the eleven session-lifecycle tool descriptors.
///
/// Why: spawning, stopping, resuming, decommissioning, hard-deleting,
/// observing, and driving a managed session are the operations the driver
/// skill needs; surfacing them as MCP tools removes the CLI-scraping defect.
/// #1508 adds two fleet-wide teardown tools so a driver can clean up its
/// throwaway test sessions and purge legacy tombstones without scraping the
/// CLI; #2012 adds `session_delete` — a single-record hard-delete distinct
/// from `session_decommission` (which stops the runtime and may remove the
/// workspace but always leaves a tombstone). The PM pause/resume context tools
/// (`session_context_catchup`, `session_context_pause`) are a DIFFERENT
/// concept — PM session snapshot/digest, not managed-sub-session lifecycle —
/// kept here anyway to avoid a third tool-group file for two tools. Keeping
/// them in their own builder keeps `tools/core.rs` and this file each well
/// under the 500-SLOC cap.
/// What: returns the eleven descriptors in catalog order. `session_new` takes the
/// repo/ref/task spawn inputs (plus optional `ephemeral`); the per-session tools
/// take a `session_id` (`session_delete` also takes optional `force`);
/// `session_decommission_ephemeral` takes no args; `session_prune` takes a
/// `state` filter plus `dry_run`/`include_active`; `session_context_catchup`
/// takes a required `project_dir` plus optional `session_id`/`all_projects`/
/// `full`; `session_context_pause` takes required `project_dir`/`session_id`/
/// `summary` plus optional bullet-list sections, `tmux_window`, and
/// `prune_worktrees`. Every schema sets `additionalProperties: false` so the
/// driver gets a clear error on a typo.
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
                    },
                    "ephemeral": {
                        "type": "boolean",
                        "description": "Tag this as an EPHEMERAL (test/throwaway) session eligible for bulk teardown and age-based auto-reap. Defaults to false (a durable session the automatic teardown paths never touch)."
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
            "session_delete",
            "Hard-delete a managed session's RECORD from the store — distinct \
             from `session_decommission`, which stops the runtime and may remove \
             the workspace but always leaves a Decommissioned tombstone behind. \
             `session_delete` permanently drops the record itself. FAIL-CLOSED: \
             a RUNNING session (Active/Provisioning) is REFUSED unless `force` is \
             true. Never touches the workspace directory on disk — this is a \
             store-only operation.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "Managed session id (UUID)."
                    },
                    "force": {
                        "type": "boolean",
                        "description": "Bypass the running-session guard and delete the record anyway. Defaults to false (the fail-closed default)."
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
        tool(
            "session_decommission_ephemeral",
            "Tear down EVERY ephemeral (test/throwaway) managed session in one \
             shot: kill each runtime, remove its workspace, and tombstone the \
             record. REAL sessions default `ephemeral=false` and are NEVER touched \
             by this tool. Returns the count decommissioned. Use this from e2e \
             harnesses or to clean up after a test run.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        tool(
            "session_prune",
            "Prune managed sessions by state and compact tombstones. `state` \
             selects which records to target: `ephemeral` (test sessions), \
             `stopped`, `decommissioned` (drop existing tombstones from the store), \
             or `all` (every NON-running record). A RUNNING session is NEVER torn \
             down unless `include_active` is true. With `dry_run` the tool REPORTS \
             what would be pruned without mutating anything. This is the tool to \
             purge legacy stale records that predate the ephemeral flag.",
            json!({
                "type": "object",
                "properties": {
                    "state": {
                        "type": "string",
                        "enum": ["ephemeral", "stopped", "decommissioned", "all"],
                        "description": "Which records to target."
                    },
                    "dry_run": {
                        "type": "boolean",
                        "description": "Report what WOULD be pruned without mutating anything (default false)."
                    },
                    "include_active": {
                        "type": "boolean",
                        "description": "Also tear down RUNNING (Active/Provisioning) sessions. Off by default — the fail-closed safety gate."
                    }
                },
                "required": ["state"],
                "additionalProperties": false
            }),
        ),
        tool(
            "session_context_catchup",
            "Return a STRUCTURED (JSON, not prose) resume digest for `project_dir`: \
             paused sessions (native trusty-mpm + legacy claude-mpm formats), \
             recent git commits, and recent memory-palace activity — the same \
             three sources `tm session catchup` renders as markdown, restructured \
             as typed fields. This is a manual PEEK: it NEVER advances the \
             incremental-catchup watermark (only automatic session-start \
             injection does that), so calling it repeatedly is always safe and \
             `watermark_advanced` in the result is always `false`.",
            json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Absolute path to the project directory to scan. Required — the stdio bridge forwards no cwd, so the daemon cannot infer it."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Optional session id used only to resolve `resolved_snapshot` (the snapshot this specific session would resume from)."
                    },
                    "all_projects": {
                        "type": "boolean",
                        "description": "Also scan every project in the legacy claude-mpm registry, merging their sessions/commits/memory into the same arrays. Defaults to false (project_dir only)."
                    },
                    "full": {
                        "type": "boolean",
                        "description": "Ignore the watermark and return full history instead of just activity since the last catch-up. Defaults to false."
                    }
                },
                "required": ["project_dir"],
                "additionalProperties": false
            }),
        ),
        tool(
            "session_context_pause",
            "Write a session-pause snapshot for `project_dir`: a \
             `session-YYYYMMDD-HHMMSS.md` file in the SAME section format the \
             catch-up reader already parses (`## Summary` / `## Completed` / \
             `## In Progress` / `## Next Steps` / `## Git Context` / \
             `## Tmux Window`), plus an appended `pause` line in the \
             append-only `sessions-log.jsonl`. Also prunes orphaned managed-session \
             git worktrees in-process (same engine as `tm session prune-worktrees`) \
             unless `prune_worktrees` is set to `false`. Does NOT touch tmux — \
             window realignment on resume stays a PM-side `tmux select-window` step.",
            json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Absolute path to the project directory to snapshot."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Stable id for the originating session; keys the append-only pause-log entry."
                    },
                    "summary": {
                        "type": "string",
                        "description": "The `## Summary` section body — required, human-readable current-state prose."
                    },
                    "completed": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Bullet items for the `## Completed` section. Omitted entirely from the file when empty."
                    },
                    "in_progress": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Bullet items for the `## In Progress` section. Omitted entirely from the file when empty."
                    },
                    "next_steps": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Bullet items for the `## Next Steps` section. Omitted entirely from the file when empty."
                    },
                    "tmux_window": {
                        "type": "string",
                        "description": "`session_name:window_index:window_id` captured via `tmux display-message`, when inside tmux. Omitted entirely from the file when absent."
                    },
                    "prune_worktrees": {
                        "type": "boolean",
                        "description": "Also prune orphaned managed-session git worktree directories as part of pause wrap-up. Defaults to true."
                    }
                },
                "required": ["project_dir", "session_id", "summary"],
                "additionalProperties": false
            }),
        ),
    ]
}
