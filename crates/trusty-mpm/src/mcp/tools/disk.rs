//! The Disk dashboard's MCP tool descriptor (#6927, DOC-73 §16.6 item 2).
//!
//! Why: DOC-73 §16.4 states trusty-console reaches trusty-mpm through its MCP
//! tools and never through the daemon's HTTP port, and no existing tool exposes
//! the merged-PR worktree survey — `session_prune` is orphan-only, and the
//! `merged_prs` argument lives on the HTTP route alone. This is that tool.
//! What: [`disk_tools`] returns the single `disk_survey` descriptor. Keeping it
//! in its own file follows the per-group split every other tool family uses and
//! keeps `tools/mod.rs` a thin facade.
//! Test: `super::tests::disk_tools_present`,
//! `super::tests::catalog_names_match_constant`,
//! `crate::mcp::tests::dispatch_disk_survey_tool`.

use serde_json::{Value, json};

use super::tool;

/// Build the one Disk-dashboard tool descriptor.
///
/// Why: the console Disk view (DOC-73 §16.3) needs projects, their worktrees,
/// each worktree's bytes, and each worktree's staleness tier from ONE call —
/// the sunburst renders all four at once.
/// What: `disk_survey`, with three optional arguments and a closed schema.
/// `project` scopes the survey to one managed project; `budget_seconds` bounds
/// the whole pass — git and `gh` subprocesses per worktree, and the byte walks,
/// which are the expensive half (#6929) — capped at
/// [`MAX_BUDGET_SECONDS`](crate::daemon::mcp_disk::MAX_BUDGET_SECONDS) because a
/// larger figure outlives the stdio bridge that carries the answer (#7313);
/// `group_by` adds the per-session roll-up (#7313).
/// Test: `super::tests::disk_tools_present`,
/// `super::tests::disk_survey_schema_round_trips`.
pub(super) fn disk_tools() -> Vec<Value> {
    vec![tool(
        "disk_survey",
        "Survey every managed project and worktree for the console Disk view: \
         per-worktree bytes (from the cached size index, with `from_cache` / \
         `truncated` / `measured_at` passed through), a staleness tier \
         (`stale` / `review` / `keep` / `missing`) and the REASONS behind it \
         (`dirty`, `unpushed`, `live-session`, `keep-list`, \
         `unknown-branch-state`, …), the reclaim gate that refused each \
         worktree, and the owner keep-list that was applied. READ-ONLY: it \
         classifies and reports, and removes nothing — clearing a worktree is a \
         separate, explicitly confirmed action. A worktree holding uncommitted \
         or unpushed work, claimed by a live session, owned by a dispatched \
         agent, or named by the operator's `disk.keep_list` config is never \
         reported `stale`. EXPENSIVE: classification runs git and `gh` \
         subprocesses per worktree AND a byte walk per project and per \
         workspace root, so bound it with `budget_seconds` when polling — the \
         budget stops both halves, worktrees past it are still listed as \
         `review`, and a project or root past it reports `bytes: null`. \
         Worktrees are measured before projects and the root, so a \
         budget-limited pass spends what it has on the rows a view colours. \
         Every row also carries `owning_session` — the live session claiming \
         it, else the session its ownership sentinel names, so an ENDED \
         session's leftovers are attributed rather than orphaned — and \
         `build_dir_bytes`, the `target*` share of its total. Pass \
         `group_by: \"session\"` for the `by_session` roll-up over those.",
        json!({
            "type": "object",
            "properties": {
                "project": {
                    "type": "string",
                    "description": "Scope to one managed project: its `<owner>/<repo>` label, its bare repository directory name, or an absolute path prefix. Omit to survey every project."
                },
                "budget_seconds": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 55,
                    "description": "Stop after this many seconds — classification AND byte measurement both (#6929). Remaining worktrees are listed as `review` with an `unknown-branch-state` reason, never as `stale`; a project or root the budget was reached before reports `bytes: null`. CLAMPED to 55 (#7313): the `tm serve --stdio` bridge every caller reaches this tool through gives up at 60 s, so a larger budget returns a transport error rather than a survey. A clamped request answers with `budget_clamped: true`. Omit for an unbounded survey, which on a large fleet outruns any caller with a request timeout."
                },
                "group_by": {
                    "type": "string",
                    "enum": ["session"],
                    "description": "Add a per-session roll-up beside the project tree (#7313), as `by_session`: one entry per owning session — total bytes, `target*` build-directory bytes, worktree count, the tier split, and the worktree paths — sorted by bytes descending, with a `session_id: null` bucket for worktrees nothing attributed. A worktree is charged to the live session claiming it, else to the session its `.trusty-mpm-worktree` sentinel names, so an ENDED session's leftovers are still attributed. Omit to leave `by_session` out of the response entirely."
                }
            },
            "additionalProperties": false
        }),
    )]
}
