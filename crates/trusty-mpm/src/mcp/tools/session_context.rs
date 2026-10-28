//! PM pause/resume context MCP tool descriptors (#10300 split from `session.rs`).
//!
//! Why: `session_context_catchup` and `session_context_pause` are PM snapshot
//! tools, not managed-session lifecycle, and their descriptions had grown to
//! 5,099 B and 2,963 B of `tools/list` text — past the client's 4,096-char
//! cut (#10300). The field reference now lives in the tm-capabilities skill
//! (`content/skills/tm-capabilities/references/workflows.md` section 6).
//! What: [`session_context_tools`] returns the two descriptors, in catalog
//! order after `session_prune`.
//! Test: `context_tool_descriptions_fit_the_client_cut`,
//! `super::tests::session_tools_present`.

use serde_json::{Value, json};

use super::tool;

/// Build the two PM pause/resume context tool descriptors.
///
/// Why: they replace the bash `tm session catchup` / git / snapshot-file
/// plumbing the `/tm-session-pause` and `/tm-session-resume` skills shelled
/// out to.
/// What: `session_context_catchup` takes a required `project_dir` plus optional
/// `session_id`/`tmux_window`/`tmux_session_created`/`all_projects`/`full`/
/// `sessions_offset`; `session_context_pause` takes required
/// `project_dir`/`summary`, an optional `session_id` (#6888), bullet-list
/// sections, `tmux_window` and `prune_worktrees`. Both schemas set
/// `additionalProperties: false`.
/// Test: `context_tool_descriptions_fit_the_client_cut`.
pub(super) fn session_context_tools() -> Vec<Value> {
    vec![
        tool(
            "session_context_catchup",
            // #10300: the field reference moved to the tm-capabilities skill
            // (references/workflows.md section 6); keep this near 600 B.
            "Return structured JSON resume context for `project_dir`. The \
             default is LEAN (#9883): the snapshot resolved for this caller \
             (`resolved_snapshot`, `resolved_via`, `resolved_session`), a live \
             `git_status`, `resolved_snapshot_superseded` with \
             `commits_since_snapshot`, the `resume_verify` re-checks and the \
             `session_refs` hydration report. `full: true` adds the paged \
             `sessions`, `recent_commits` and `recent_memory`; follow \
             `sessions_next_offset`. No call advances a watermark. Act on a \
             `resume_verify` `returned` head, never a snapshot head, and read \
             `unverified` as unknown. Field reference: the tm-capabilities \
             skill, `references/workflows.md` section 6.",
            json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Absolute path to the project directory to scan. Required — the stdio bridge forwards no cwd, so the daemon cannot infer it."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Session id that `resolved_snapshot` is resolved FOR. Omit it and the daemon derives the same id `session_context_pause` derived for you, so a pause written with no `session_id` resolves back here exactly (#6888); with neither an id nor a `tmux_window` an unidentified caller still owns nothing in a shared store. Naming another session's id is the explicit opt-in that reads that session's state (#5272)."
                    },
                    "tmux_window": {
                        "type": "string",
                        "description": "The CALLER's own `session_name:window_index:window_id` from `tmux display-message`, e.g. `tm-dogfood:0:@230`. Used only when `session_id` resolved nothing: matches the `window_id` (`@230`) component against snapshots this project paused, newest first. When no window matches — a relaunch recreates the window with a new id — the session name (`tm-dogfood`) is matched instead (#8408), only together with `tmux_session_created`; a digits-only session name never matches. Window indexes are never matched."
                    },
                    "tmux_session_created": {
                        "type": "integer",
                        "description": "The CALLER's tmux session creation time, `tmux display-message -p '#{session_created}'` (Unix seconds). trusty-mpm names every tmux session `tm-<folder>`, so a later session reuses the name; the session-name route matches only snapshots paused after this time, and resolves nothing without it (#8408)."
                    },
                    "all_projects": {
                        "type": "boolean",
                        "description": "Also scan every project in the legacy claude-mpm registry, merging their sessions/commits/memory into the same arrays. Full digest only: requires `full: true`, else the call errors. Defaults to false (project_dir only)."
                    },
                    "full": {
                        "type": "boolean",
                        "description": "Return the full digest — paused sessions, recent commits and recent memory over the whole history, with no watermark — instead of the lean default (the resolved snapshot plus live `git_status`). Defaults to false. The full digest is paged, not dropped — walk `sessions_next_offset` to read all of it."
                    },
                    "sessions_offset": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Full digest only (a non-zero value without `full: true` errors). Index into the ordered session list this page starts at. Defaults to 0, the page a resume wants — it holds the sessions you own. Pass the previous response's `sessions_next_offset` to continue; when that field is null you have the whole history. The offset is POSITIONAL into a list rebuilt from disk on every call, so a snapshot paused between two calls shifts the indexes and a later page can repeat a record you already have — de-duplicate by `source_file` or `paused_at` if that matters to you (#5557)."
                    }
                },
                "required": ["project_dir"],
                "additionalProperties": false
            }),
        ),
        tool(
            "session_context_pause",
            // #10300: the field reference moved to the tm-capabilities skill
            // (references/workflows.md section 6); keep this near 600 B.
            "Write a session-pause snapshot for `project_dir` that only this \
             session can resume (#5272). Leave `session_id` out: the daemon \
             derives it, and an invented id files the snapshot where \
             `session_context_catchup` never looks (#6888). The pause also \
             opens a docs-only PR when the project tracks \
             `.trusty-mpm/sessions/` (`snapshot_publish`), appends the snapshot \
             to this session's git ref (`ref_name`; report a `ref_error`, never \
             retry), writes `resume_slot`, and prunes orphaned worktrees, never \
             a dirty one (`skipped_dirty_worktrees`). Does NOT touch tmux. \
             Field reference: the tm-capabilities skill, \
             `references/workflows.md` section 6.",
            json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Absolute path to the project directory to snapshot."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "OPTIONAL — omit it and the daemon derives the id from your own identity, which is what the next `session_context_catchup` looks the snapshot back up by (#6888). Do NOT invent a stable-looking string: the resume lookup is exact string equality, so a second guess never matches the first. Pass it only to attribute the pause to a specific KNOWN id. The id actually used comes back in the response's `session_id`."
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
                // #6888: `session_id` is derived when omitted, so it is not required.
                "required": ["project_dir", "summary"],
                "additionalProperties": false
            }),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Client cut: a description longer than this is truncated by the client (#10300).
    const CLIENT_DESCRIPTION_CUT: usize = 4096;

    /// The #10300 closure budget for each context-tool description.
    const CONTEXT_DESCRIPTION_BUDGET: usize = 700;

    /// #10300: both descriptions stay under the client cut and the ~600 B budget.
    #[test]
    fn context_tool_descriptions_fit_the_client_cut() {
        let tools = session_context_tools();
        assert_eq!(tools.len(), 2);
        for tool in &tools {
            let name = tool["name"].as_str().expect("tool has a name");
            let len = tool["description"]
                .as_str()
                .expect("tool has a description")
                .len();
            assert!(
                len < CLIENT_DESCRIPTION_CUT,
                "{name}: description is {len} B, at or over the {CLIENT_DESCRIPTION_CUT} B client cut"
            );
            assert!(
                len <= CONTEXT_DESCRIPTION_BUDGET,
                "{name}: description is {len} B, over the {CONTEXT_DESCRIPTION_BUDGET} B budget; \
                 move the detail to references/workflows.md"
            );
        }
    }
}
