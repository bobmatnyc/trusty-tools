# MCP Tool Reference

Generated from `trusty_mpm::mcp::tools::tool_catalog()` — trusty-mpm's own MCP tool surface (`tools/list` over the `serve --stdio` bridge), in catalog order. Regenerate with `tm generate capabilities`.

33 tools.

## `session_list`

List all Claude Code sessions the trusty-mpm daemon is managing, with status, working directory, and active delegation count.

No parameters.

## `session_status`

Get detailed status for one session: uptime, token usage, current agent, memory pressure, and last activity.

| Parameter | Type | Required |
|---|---|---|
| `session_id` | `string` | yes |

## `agent_delegate`

Record and gate a delegation to a named agent: applies the circuit-breaker and depth limits and adds the delegation to the session's dashboard tree. This is a TRACKING/GATING companion, NOT an execution path — it does not spawn the agent. Execution happens via the native Agent/Task tool using the deployed agent name (from ~/.claude/agents/), e.g. Agent(subagent_type="rust-engineer").

| Parameter | Type | Required |
|---|---|---|
| `agent` | `string` | yes |
| `session_id` | `string` | yes |
| `task` | `string` | yes |
| `tier` | `string` | no |

## `memory_protect`

Report current context-window token usage for a session. The daemon classifies pressure (ok/warn/alert/compact) and may trigger a trusty-memory snapshot or auto-compaction.

| Parameter | Type | Required |
|---|---|---|
| `session_id` | `string` | yes |
| `used_tokens` | `integer` | yes |
| `window_tokens` | `integer` | yes |

## `circuit_breaker_status`

Inspect circuit-breaker state. With no `agent`, returns every agent's breaker; with `agent`, returns just that one.

| Parameter | Type | Required |
|---|---|---|
| `agent` | `string` | no |

## `hook_event`

Forward a Claude Code hook event to the daemon's observability pipeline (live dashboard feed, Telegram alerts, memory tracking).

| Parameter | Type | Required |
|---|---|---|
| `event` | `string` | yes |
| `payload` | `any` | no |
| `session_id` | `string` | yes |

## `list_recent_errors`

List recently captured ERROR-level events across all trusty-* daemons (trusty-search, trusty-memory, trusty-analyze, trusty-mpm). Each entry includes a fingerprint for deduplication, an occurrence count, the originating crate, and a one-line summary. Use `preview_bug_report` to see the full scrubbed body before filing.

| Parameter | Type | Required |
|---|---|---|
| `limit` | `integer` | no |

## `preview_bug_report`

Preview the exact scrubbed GitHub issue body that would be filed for a specific error fingerprint. Shows what data is included, what was redacted (paths, tokens, secrets), and the proposed labels. Nothing is filed — call `report_bug` with `confirm: true` to actually file.

| Parameter | Type | Required |
|---|---|---|
| `fingerprint` | `string` | yes |

## `report_bug`

File a GitHub issue in bobmatnyc/trusty-tools for the error identified by `fingerprint`. Requires explicit user consent: `confirm` must be true or nothing is filed. If an open issue with the same fingerprint already exists, posts a '+1 occurrence' comment instead of creating a duplicate. Returns `{ filed, deduped, issue_url, issue_number }` on success, or an actionable error message if no token is configured (set TRUSTY_BUGREPORT_GITHUB_TOKEN). Always call `preview_bug_report` first so the user can review the scrubbed content.

| Parameter | Type | Required |
|---|---|---|
| `confirm` | `boolean` | yes |
| `fingerprint` | `string` | yes |

## `session_new`

Spawn a new managed Claude Code (or trusty-code) session in an isolated, freshly-provisioned workspace cloned from `repo_url` at `ref`. The daemon creates the tmux host, deploys agents/skills, and launches the harness with the given `task`. Returns the new managed session id, tmux name, workspace path, lifecycle state, and the `tmux attach-session` command.

| Parameter | Type | Required |
|---|---|---|
| `ephemeral` | `boolean` | no |
| `name_hint` | `string` | no |
| `ref` | `string` | yes |
| `repo_url` | `string` | yes |
| `runtime` | `string` | no |
| `task` | `string` | yes |

## `session_stop`

Stop a managed session's runtime (kills the tmux session and harness process) while PRESERVING its workspace on disk and its record, so it can be resumed later with `session_resume`. This is NOT a teardown — use `session_decommission` to remove the workspace permanently.

| Parameter | Type | Required |
|---|---|---|
| `session_id` | `string` | yes |

## `session_resume`

Resume a previously-stopped managed session: re-create the tmux host rooted at the still-on-disk workspace and re-spawn the SAME runtime backend the session was created with (no re-clone). Returns the updated session record.

| Parameter | Type | Required |
|---|---|---|
| `session_id` | `string` | yes |

## `session_decommission`

Permanently tear down a managed session: kill the runtime, REMOVE the workspace directory from disk, and mark the record Decommissioned. This is terminal — the session can NOT be resumed afterwards. A tombstone record is retained for audit.

| Parameter | Type | Required |
|---|---|---|
| `session_id` | `string` | yes |

## `session_delete`

Hard-delete a managed session's RECORD from the store — distinct from `session_decommission`, which stops the runtime and may remove the workspace but always leaves a Decommissioned tombstone behind. `session_delete` permanently drops the record itself. FAIL-CLOSED: a RUNNING session (Active/Provisioning) is REFUSED unless `force` is true. Never touches the workspace directory on disk — this is a store-only operation.

| Parameter | Type | Required |
|---|---|---|
| `force` | `boolean` | no |
| `session_id` | `string` | yes |

## `session_activity`

Inspect a managed session's recent activity. ALWAYS returns the raw tmux pane content (last `lines` lines, default 60) plus structured lifecycle fields (`runtime_active`, `pending_decision`, `proposed_default`) so the caller can do its own inference WITHOUT an LLM key. When OPENROUTER_API_KEY is configured the daemon also returns an LLM `classification` of the session state.

| Parameter | Type | Required |
|---|---|---|
| `lines` | `integer` | no |
| `session_id` | `string` | yes |

## `session_send`

Send a line of text into a managed session's tmux pane (followed by Enter), e.g. to answer a prompt or drive the harness. Returns a confirmation with the target tmux session name.

| Parameter | Type | Required |
|---|---|---|
| `session_id` | `string` | yes |
| `text` | `string` | yes |

## `session_decommission_ephemeral`

Tear down EVERY ephemeral (test/throwaway) managed session in one shot: kill each runtime, remove its workspace, and tombstone the record. REAL sessions default `ephemeral=false` and are NEVER touched by this tool. Returns the count decommissioned. Use this from e2e harnesses or to clean up after a test run.

No parameters.

## `session_prune`

Prune managed sessions by state and compact tombstones. `state` selects which records to target: `ephemeral` (test sessions), `stopped`, `decommissioned` (drop existing tombstones from the store), or `all` (every NON-running record). A RUNNING session is NEVER torn down unless `include_active` is true. With `dry_run` the tool REPORTS what would be pruned without mutating anything. This is the tool to purge legacy stale records that predate the ephemeral flag.

| Parameter | Type | Required |
|---|---|---|
| `dry_run` | `boolean` | no |
| `include_active` | `boolean` | no |
| `state` | `string` | yes |

## `session_context_catchup`

Return a STRUCTURED (JSON, not prose) resume digest for `project_dir`: paused sessions (native trusty-mpm + legacy claude-mpm formats), recent git commits, and recent memory-palace activity — the same three sources `tm session catchup` renders as markdown, restructured as typed fields. This is a manual PEEK: it NEVER advances the incremental-catchup watermark (only automatic session-start injection does that), so calling it repeatedly is always safe and `watermark_advanced` in the result is always `false`.

| Parameter | Type | Required |
|---|---|---|
| `all_projects` | `boolean` | no |
| `full` | `boolean` | no |
| `project_dir` | `string` | yes |
| `session_id` | `string` | no |

## `session_context_pause`

Write a session-pause snapshot for `project_dir`: a `session-YYYYMMDD-HHMMSS.md` file in the SAME section format the catch-up reader already parses (`## Summary` / `## Completed` / `## In Progress` / `## Next Steps` / `## Git Context` / `## Tmux Window`), plus an appended `pause` line in the append-only `sessions-log.jsonl`. Also prunes orphaned managed-session git worktrees in-process (same engine as `tm session prune-worktrees`) unless `prune_worktrees` is set to `false`. Does NOT touch tmux — window realignment on resume stays a PM-side `tmux select-window` step.

| Parameter | Type | Required |
|---|---|---|
| `completed` | `array` | no |
| `in_progress` | `array` | no |
| `next_steps` | `array` | no |
| `project_dir` | `string` | yes |
| `prune_worktrees` | `boolean` | no |
| `session_id` | `string` | yes |
| `summary` | `string` | yes |
| `tmux_window` | `string` | no |

## `console_metrics`

Return the standard trusty-console metrics report for trusty-mpm: service id, display name, version, coarse health status, and a `metrics` payload carrying the managed-session fleet snapshot (counts by lifecycle state) and the supervisor auto-resume control state. Polled uniformly by trusty-console for the dashboard.

No parameters.

## `supervisor_status`

Return the managed-session fleet snapshot and the supervisor auto-resume control state as `{ fleet, auto_resume }`. `fleet` carries counts by lifecycle state (provisioning/active/stopped/errored/decommissioned), pending decisions, and last activity; `auto_resume` carries the persisted desired flag, the supervisor's boot-time env flag, and whether a restart is pending.

No parameters.

## `auto_resume_set`

Enable or disable supervisor auto-resume by persisting the operator's desired flag to `~/.trusty-mpm/auto_resume`. The 24/7 supervisor reads this on its next sweep; the env var the supervisor booted with stays in force until then (the response's `pending_restart` flags the difference). This is the console's non-CLI auto-resume control.

| Parameter | Type | Required |
|---|---|---|
| `enabled` | `boolean` | yes |

## `config_read`

Read trusty-mpm's `~/.trusty-tools/trusty-mpm/config.yaml` (the #1220 cross-crate config convention) and return its current settings as JSON: `workspace_root_template`, `auto_resume`, `default_model`, the global `github` GitHub-identity binding, the global `untracked_sync` untracked/secret-file sync allowlist (#2196), and the full `projects` list (each entry may carry its own `github`/`commit_name`/`commit_email`/ `untracked_sync` per-project override). An absent file returns the defaults (all null/empty). Backs the trusty-console Config tab.

No parameters.

## `config_write`

Write trusty-mpm's `~/.trusty-tools/trusty-mpm/config.yaml` (#1220, #2184). Supplied fields replace the corresponding settings; omitted fields are left unchanged. `workspace_root_template` sets the managed-session workspace root (a leading `~` is expanded); `auto_resume` sets the supervisor default; `default_model` sets the launch model. The `github_*` fields set a GitHub-CLI identity binding (`config_dir` > `token_env` > `account`, plus `host`) — with no `project_name` they set the GLOBAL binding; with `project_name` set to an ALREADY-REGISTERED project (via `project_register` or a static `config.projects` entry) they set that project's OWN binding instead, which takes precedence over the global one for that project's `gh` calls. `commit_name`/`commit_email` set a per-project git commit-author override applied to managed/provisioner git operations — they REQUIRE `project_name` (commit identity has no global tier). `untracked_sync_patterns`/ `untracked_sync_enabled` (#2196) set the allowlist of untracked/secret filename patterns (default `.env*`) synced from the operator's live checkout into each session worktree at spawn, and the on/off toggle for that sync; like `github_*` (and unlike commit identity) these DO have a global tier — omit `project_name` to set it, or supply an ALREADY-REGISTERED `project_name` to override it for that project only. Returns the merged config that was persisted.

| Parameter | Type | Required |
|---|---|---|
| `auto_resume` | `boolean` | no |
| `commit_email` | `string` | no |
| `commit_name` | `string` | no |
| `default_model` | `string` | no |
| `github_account` | `string` | no |
| `github_config_dir` | `string` | no |
| `github_host` | `string` | no |
| `github_token_env` | `string` | no |
| `project_name` | `string` | no |
| `untracked_sync_enabled` | `boolean` | no |
| `untracked_sync_patterns` | `array` | no |
| `workspace_root_template` | `string` | no |

## `project_list`

List all projects registered in the project registry. Returns a JSON array of project objects, each with `name`, `repo_url`, `default_branch`, and optional `stack_hint`, `tags`, `description`, and `gh_user` (the project's preferred GitHub account login, #2081).

No parameters.

## `project_register`

Register or update a project in the project registry. Registration is idempotent — calling with the same `name` updates the existing entry rather than creating a duplicate. Returns the registered project record.

| Parameter | Type | Required |
|---|---|---|
| `default_branch` | `string` | no |
| `description` | `string` | no |
| `gh_account` | `string` | no |
| `gh_user` | `string` | no |
| `name` | `string` | yes |
| `repo_url` | `string` | yes |
| `stack_hint` | `string` | no |
| `tags` | `array` | no |

## `project_get`

Look up a single project by name. Returns the project record (`name`, `repo_url`, `default_branch`, and optional fields including `gh_user`, the project's preferred GitHub account login) or an error when the name is not found in the registry.

| Parameter | Type | Required |
|---|---|---|
| `name` | `string` | yes |

## `project_resolve`

Resolve a natural-language query to the best-matching registered project. Accepts free-text task descriptions, GitHub URLs, ticket IDs (e.g. PROJ-123), project names, keywords, or tags. Returns a `primary` match with confidence score and reason, a `needs_disambiguation` flag (true when multiple candidates score above the disambiguation floor), and a ranked `matches` list. Confidence is always in [0.0, 1.0]. On no match, `primary` is null and an `error` field explains the failure.

| Parameter | Type | Required |
|---|---|---|
| `query` | `string` | yes |

## `session_proxy_focus`

Focus a conversation on ONE managed session so later `session_proxy_message` calls route to it without repeating the id. `conversation_key` is any opaque string you choose to identify this conversation (it is shared with the Telegram/HTTP proxy surfaces — the same key sees the same focus). Pass `session_id` (a managed session id, friendly name, or unambiguous prefix) to SET the focus; OMIT it (or pass empty) to QUERY the current focus without changing it. Returns a tagged `outcome`: `focused` (set), `current` (query — `target` may be null), or `not_found` (unresolved; focus unchanged).

| Parameter | Type | Required |
|---|---|---|
| `conversation_key` | `string` | yes |
| `session_id` | `string` | no |

## `session_proxy_unfocus`

Clear a conversation's focus so free text no longer injects into any session (back to fleet-wide chat). Reports the session that was cleared, or null if nothing was focused. Never an error — unfocusing an already-unfocused conversation is a harmless no-op.

| Parameter | Type | Required |
|---|---|---|
| `conversation_key` | `string` | yes |

## `session_proxy_message`

INJECT free text into the conversation's focused session (equivalent to `session_send` but addressed by focus instead of an explicit id). Focus the conversation first with `session_proxy_focus`. Returns a tagged `outcome`: `sent` on success; `auto_unfocused` if the focused session had vanished (focus is cleared for you); `failed` on a transient error (focus preserved); or `no_focus` if nothing was focused — in which case handle the text yourself rather than treating it as an error.

| Parameter | Type | Required |
|---|---|---|
| `conversation_key` | `string` | yes |
| `text` | `string` | yes |

## `session_proxy_summary`

SUMMARIZE the conversation's focused session — a lightweight digest of what it is doing (lifecycle state, a short activity summary, and any pending decision it is blocked on) WITHOUT attaching to tmux or requiring an LLM key. Focus the conversation first with `session_proxy_focus`. Returns a tagged `outcome`: `summary` on success; `auto_unfocused` if the session had vanished; `failed` on a transient error; or `no_focus` if nothing was focused.

| Parameter | Type | Required |
|---|---|---|
| `conversation_key` | `string` | yes |

## Sibling Daemon MCP Surfaces

Each sibling daemon owns its own tool catalog in its own crate — out of scope for direct extraction here. See that daemon's own descriptor source for its exact tool list:

- **trusty-search** — `crates/trusty-search/src/mcp/tools/descriptors.rs`
- **trusty-analyze** — `crates/trusty-analyze/src/mcp/descriptors.rs`
- **trusty-memory** — `crates/trusty-memory/src/mcp_service.rs`
