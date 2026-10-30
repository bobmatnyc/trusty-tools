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
//! What: [`session_tools`] returns the ten `{ name, description, inputSchema }`
//! descriptors — `session_new`, `session_stop`, `session_resume`,
//! `session_decommission`, `session_delete` (#2012),
//! `session_delete_records` (#6431), `session_activity`,
//! `session_send`, and the #1508 fleet-wide `session_decommission_ephemeral` +
//! `session_prune`. The PM pause/resume context tools live in the sibling
//! [`super::session_context`] (#10300). The shared [`tool`] builder is
//! re-exported from the parent module.
//! Test: `cargo test -p trusty-mpm` (in [`super`]) asserts the full catalog is
//! well-formed and that each of these names is present.

use serde_json::{Value, json};

use super::tool;

/// Build the ten session-lifecycle tool descriptors.
///
/// Why: spawning, stopping, resuming, decommissioning, hard-deleting,
/// observing, and driving a managed session are the operations the driver
/// skill needs; surfacing them as MCP tools removes the CLI-scraping defect.
/// #1508 adds two fleet-wide teardown tools so a driver can clean up its
/// throwaway test sessions and purge legacy tombstones without scraping the
/// CLI; #2012 adds `session_delete` — a single-record hard-delete distinct
/// from `session_decommission` (which stops the runtime and may remove the
/// workspace but always leaves a tombstone); #6431 adds
/// `session_delete_records`, the record-only bulk delete the console's Sessions
/// tab drives. The PM pause/resume context tools are a different concept and
/// live in [`super::session_context`] (#10300: split to hold the SLOC cap).
/// What: returns the ten descriptors in catalog order. `session_new` takes the
/// repo/ref/task spawn inputs (plus optional `ephemeral` and the
/// bridge-stamped `parent_session_id`, #10476); the per-session tools
/// take a `session_id` (`session_delete` also takes optional `force`);
/// `session_delete_records` takes a non-empty `session_ids` array;
/// `session_decommission_ephemeral` takes no args; `session_prune` takes a
/// `state` filter plus `dry_run`/`include_active`. Every schema sets
/// `additionalProperties: false` so the
/// driver gets a clear error on a typo.
/// Test: `super::tests::session_tools_present`,
/// `super::tests::catalog_names_match_constant`,
/// `super::tests::session_new_schema_lists_parent_and_keeps_additional_properties_false`.
pub(super) fn session_tools() -> Vec<Value> {
    vec![
        tool(
            "session_new",
            "Spawn a new managed Claude Code (or trusty-code) session for \
             `repo_url` at `ref`. `repo_url` MUST be an absolute path to a \
             directory that already exists on the daemon host; the session runs \
             on that main checkout itself (ADR-0037). A remote URL is an error — \
             trusty-mpm clones no repository and creates no worktree for a \
             session (ADR-0055); clone it yourself first and pass the path. A \
             main-checkout session may write documents and configuration only, \
             and the writers it dispatches are given their own worktrees \
             (ADR-0044, ADR-0048). The daemon creates the tmux host, deploys \
             agents/skills, and launches the harness with the given `task`. \
             Returns the new managed session id, tmux name, workspace path, \
             lifecycle state, and the `tmux attach-session` command.",
            json!({
                "type": "object",
                "properties": {
                    "repo_url": {
                        "type": "string",
                        "description": "Absolute path to an EXISTING local directory the session runs in. A remote URL is refused (ADR-0055)."
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
                    },
                    "parent_session_id": {
                        "type": "string",
                        "description": "Managed id of the calling session. The `tm serve --stdio` bridge sets this from its TM_MANAGED_SESSION_ID and overwrites any value a caller sends; callers must not set it. An unknown or invalid id is ignored."
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
             tombstone record is retained for audit. A workspace holding work \
             (dirty files, unpushed commits, run output) is kept: the result's \
             `workspace_removed` is false and `workspace_kept_reason` says why.",
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
            "session_delete_records",
            "Delete a caller-supplied set of session RECORDS, and nothing else \
             (#6431). Each id is routed to whichever registry owns it: a managed \
             record is soft-deleted via the same path as `session_delete`, and a \
             legacy in-memory registry entry (the records that carry `status` \
             and no `state`, which the console buckets as \"unknown\") is \
             dropped from the registry. NEVER removes a worktree, a workspace \
             directory, or any other filesystem state, and never kills a tmux \
             host. FAIL-CLOSED: a session that is still RUNNING is REFUSED in \
             both registries (there is no `force` — stop it, or use \
             `session_delete`), a liveness probe that cannot reach a verdict \
             refuses too, and a malformed, unknown, or refused id is reported \
             as one failed row rather than counted as a deletion. Takes \
             explicit ids, never a filter, so the set an operator confirmed is \
             exactly the set that is deleted. When a deleted legacy entry \
             shares its tmux name with a live managed record, that record is \
             REPORTED as `managed_sibling` and left untouched — this tool never \
             deletes a record the caller did not name. Returns \
             `{ requested, deleted, failed, results }`.",
            json!({
                "type": "object",
                "properties": {
                    "session_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1,
                        "description": "Session ids (UUIDs) to delete records for. Duplicates are collapsed."
                    }
                },
                "required": ["session_ids"],
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
            // #5699: `sent` alone could not fail, so the description now names
            // the observed verdict a caller must read instead.
            "Send a line of text into a managed session's tmux pane (followed by \
             Enter), e.g. to answer a prompt or drive the harness. Returns the \
             target tmux session name plus `submit_state`: `submitted`, \
             `unsubmitted_paste` (the harness collapsed a long message into a \
             bracketed paste and the Enter did not commit it — resend in \
             smaller parts), or `unverified` (the pane could not be read). \
             `sent` is false only for `unsubmitted_paste`.",
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
    ]
}
