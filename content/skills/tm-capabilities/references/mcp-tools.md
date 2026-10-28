# MCP Tool Reference

Generated from `trusty_mpm::mcp::tools::tool_catalog()` — trusty-mpm's own MCP tool surface (`tools/list` over the `serve --stdio` bridge), in catalog order. Regenerate with `tm generate capabilities`.

39 tools.

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

Spawn a new managed Claude Code (or trusty-code) session for `repo_url` at `ref`. `repo_url` MUST be an absolute path to a directory that already exists on the daemon host; the session runs on that main checkout itself (ADR-0037). A remote URL is an error — trusty-mpm clones no repository and creates no worktree for a session (ADR-0055); clone it yourself first and pass the path. A main-checkout session may write documents and configuration only, and the writers it dispatches are given their own worktrees (ADR-0044, ADR-0048). The daemon creates the tmux host, deploys agents/skills, and launches the harness with the given `task`. Returns the new managed session id, tmux name, workspace path, lifecycle state, and the `tmux attach-session` command.

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

Permanently tear down a managed session: kill the runtime, REMOVE the workspace directory from disk, and mark the record Decommissioned. This is terminal — the session can NOT be resumed afterwards. A tombstone record is retained for audit. A workspace holding work (dirty files, unpushed commits, run output) is kept: the result's `workspace_removed` is false and `workspace_kept_reason` says why.

| Parameter | Type | Required |
|---|---|---|
| `session_id` | `string` | yes |

## `session_delete`

Hard-delete a managed session's RECORD from the store — distinct from `session_decommission`, which stops the runtime and may remove the workspace but always leaves a Decommissioned tombstone behind. `session_delete` permanently drops the record itself. FAIL-CLOSED: a RUNNING session (Active/Provisioning) is REFUSED unless `force` is true. Never touches the workspace directory on disk — this is a store-only operation.

| Parameter | Type | Required |
|---|---|---|
| `force` | `boolean` | no |
| `session_id` | `string` | yes |

## `session_delete_records`

Delete a caller-supplied set of session RECORDS, and nothing else (#6431). Each id is routed to whichever registry owns it: a managed record is soft-deleted via the same path as `session_delete`, and a legacy in-memory registry entry (the records that carry `status` and no `state`, which the console buckets as "unknown") is dropped from the registry. NEVER removes a worktree, a workspace directory, or any other filesystem state, and never kills a tmux host. FAIL-CLOSED: a session that is still RUNNING is REFUSED in both registries (there is no `force` — stop it, or use `session_delete`), a liveness probe that cannot reach a verdict refuses too, and a malformed, unknown, or refused id is reported as one failed row rather than counted as a deletion. Takes explicit ids, never a filter, so the set an operator confirmed is exactly the set that is deleted. When a deleted legacy entry shares its tmux name with a live managed record, that record is REPORTED as `managed_sibling` and left untouched — this tool never deletes a record the caller did not name. Returns `{ requested, deleted, failed, results }`.

| Parameter | Type | Required |
|---|---|---|
| `session_ids` | `array` | yes |

## `session_activity`

Inspect a managed session's recent activity. ALWAYS returns the raw tmux pane content (last `lines` lines, default 60) plus structured lifecycle fields (`runtime_active`, `pending_decision`, `proposed_default`) so the caller can do its own inference WITHOUT an LLM key. When OPENROUTER_API_KEY is configured the daemon also returns an LLM `classification` of the session state.

| Parameter | Type | Required |
|---|---|---|
| `lines` | `integer` | no |
| `session_id` | `string` | yes |

## `session_send`

Send a line of text into a managed session's tmux pane (followed by Enter), e.g. to answer a prompt or drive the harness. Returns the target tmux session name plus `submit_state`: `submitted`, `unsubmitted_paste` (the harness collapsed a long message into a bracketed paste and the Enter did not commit it — resend in smaller parts), or `unverified` (the pane could not be read). `sent` is false only for `unsubmitted_paste`.

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

Return structured JSON resume context for `project_dir`. The default is LEAN (#9883): the snapshot resolved for this caller (`resolved_snapshot`, `resolved_via`, `resolved_session`), a live `git_status`, `resolved_snapshot_superseded` with `commits_since_snapshot`, the `resume_verify` re-checks and the `session_refs` hydration report. `full: true` adds the paged `sessions`, `recent_commits` and `recent_memory`; follow `sessions_next_offset`. No call advances a watermark. Act on a `resume_verify` `returned` head, never a snapshot head, and read `unverified` as unknown. Field reference: the tm-capabilities skill, `references/workflows.md` section 6.

| Parameter | Type | Required |
|---|---|---|
| `all_projects` | `boolean` | no |
| `full` | `boolean` | no |
| `project_dir` | `string` | yes |
| `session_id` | `string` | no |
| `sessions_offset` | `integer` | no |
| `tmux_session_created` | `integer` | no |
| `tmux_window` | `string` | no |

## `session_context_pause`

Write a session-pause snapshot for `project_dir` that only this session can resume (#5272). Leave `session_id` out: the daemon derives it, and an invented id files the snapshot where `session_context_catchup` never looks (#6888). The pause also opens a docs-only PR when the project tracks `.trusty-mpm/sessions/` (`snapshot_publish`), appends the snapshot to this session's git ref (`ref_name`; report a `ref_error`, never retry), writes `resume_slot`, and prunes orphaned worktrees, never a dirty one (`skipped_dirty_worktrees`). Does NOT touch tmux. Field reference: the tm-capabilities skill, `references/workflows.md` section 6.

| Parameter | Type | Required |
|---|---|---|
| `completed` | `array` | no |
| `in_progress` | `array` | no |
| `next_steps` | `array` | no |
| `project_dir` | `string` | yes |
| `prune_worktrees` | `boolean` | no |
| `session_id` | `string` | no |
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

## `disk_survey`

Survey every managed project and worktree for the console Disk view: per-worktree bytes (from the cached size index, with `from_cache` / `truncated` / `measured_at` passed through), a staleness tier (`stale` / `review` / `keep` / `missing`) and the REASONS behind it (`dirty`, `unpushed`, `live-session`, `keep-list`, `unknown-branch-state`, …), the reclaim gate that refused each worktree, and the owner keep-list that was applied. READ-ONLY: it classifies and reports, and removes nothing — clearing a worktree is a separate, explicitly confirmed action. A worktree holding uncommitted or unpushed work, claimed by a live session, owned by a dispatched agent, or named by the operator's `disk.keep_list` config is never reported `stale`. EXPENSIVE: classification runs git and `gh` subprocesses per worktree AND a byte walk per project and per workspace root, so bound it with `budget_seconds` when polling — the budget stops both halves, worktrees past it are still listed as `review`, and a project or root past it reports `bytes: null`. Worktrees are measured before projects and the root, so a budget-limited pass spends what it has on the rows a view colours. A pass that runs out of budget starts one unbudgeted background pass, and later budgeted calls are answered from the last complete pass at once (#8985): `freshness` (`live`, `cached`, `partial`), `age_seconds` and `background_pass` (`running`, `idle`) say which answer this is. Every row also carries `owning_session` — the live session claiming it, else the session its ownership sentinel names, so an ENDED session's leftovers are attributed rather than orphaned — and `build_dir_bytes`, the `target*` share of its total. Pass `group_by: "session"` for the `by_session` roll-up over those.

| Parameter | Type | Required |
|---|---|---|
| `budget_seconds` | `integer` | no |
| `group_by` | `string` | no |
| `project` | `string` | no |

## `secrets_get_ref`

Resolve a secret key to its `secret://` reference for one project, on demand, through the trusty-secrets socket. NEVER returns a value, a masked head or a length: the answer holds names, a flag and a timestamp only, so a reference can go in a `.env` file or an `exec --env` flag without the value entering the transcript. The key is looked up in the names-only index: the project scope first, then the owner scope, unless `key` is an explicit `secret://<owner>/KEY` or `secret://<owner>/<repo>/KEY` reference. Returns `{ reference, key, present, scope, vault, backend, imported_at }`: `reference` (string) is the canonical reference; `key` (string) the key name; `present` (bool) whether a value is stored; `scope` (`project` | `owner` | null) and `vault` (string | null) say where; `backend` (string) is the backend the project's config selects; `imported_at` (integer Unix seconds | null) is the key's last write. An unknown key is `present: false`, not an error. Errors are fixed text: a relative `project`, an invalid key, an unreachable socket, or a project whose scopes cannot be derived from its git remote.

| Parameter | Type | Required |
|---|---|---|
| `key` | `string` | yes |
| `project` | `string` | yes |

## `ask_owner`

Ask the owner one question as a Slack DM, through the daemon's bot. Only the live managed Architect session may ask: the daemon proves the caller from the calling process, and refuses everyone else. The recipient must be a Slack user id (`U…`) that a channel route names; a channel or group-DM id is refused. Returns `{ question_id, route, dm_channel_id, thread_ts }` once the question is posted and recorded; the owner answers by picking one of the given `options` (2 to 6 labels, posted numbered from 1 and echoed in the result); free text is not delivered back to the caller. Every refusal sends nothing and names its rule (`asker_unproven`, `recipient_not_user_id`, `options_invalid`, `no_route`, `snapshot_stale`, `user_token_refused`, `table_unavailable`, ...). `send_outcome_unknown` means the question may have been posted; its id is never reused.

| Parameter | Type | Required |
|---|---|---|
| `options` | `array` | no |
| `question` | `string` | yes |
| `recipient` | `string` | yes |

## `sessions_chat`

Send one chat turn to the session coordinator, the same turn as the daemon's `mpm.sessions.chat` method. A `@<session>: <command>` message is sent to that session's pane; any other message is answered by the Session Manager, or by the legacy LLM chat when the Session Manager is off. Returns `{ reply, conv_id, actions_taken, cost }` (plus `routed_to_session` and `command_output` for a routed command); absent fields are omitted. Pass the returned `conv_id` to continue the same conversation. `actions: true` lets the Session Manager run session verbs inside the turn, which can take minutes, up to a 20-minute ceiling at the `tm serve --stdio` bridge. Only a bridge started with `--allow-chat` serves this tool. A turn holds that bridge until it ends, and a shorter caller timeout does not cancel it: use a dedicated MCP handle, a call timeout of at least 20 minutes, and do not retry. With no model configured the call fails with a `503` error text.

| Parameter | Type | Required |
|---|---|---|
| `actions` | `boolean` | no |
| `conv_id` | `string` | no |
| `history` | `array` | no |
| `message` | `string` | yes |

## `dbatg`

Write or read this PM session's Intent Ledger (goals G, decisions D, asks A, tasks T) in `<project>/.trusty-mpm/dbatg/<session>/`. `op` picks the method: `init` (`project`, `session`, `code`: create the store; same code and session again is a no-op), `batch` (`store`, `request_id`, optional `if_v`, `ops`: apply every op or none; a `request_id` sent again returns the first result and applies nothing), `show` (`store`, optional `brief`). Batch ops, tagged by `op`: `add` {kind G|D|A|T, title, goal, rank, finish, maps, status, to, opts [{l, rec}], id}, `update` {id, title, rank, esc, answer, to, outcome, status, note}, `close` {id, outcome}, `session` {task, next, waiting_on, owed, agents, crons, refs, boundary, ctx_use}, `prune` {dry_run, answered_hours, require_outcome}. Returns `init` {store, v}, `batch` {v, events [{id, ev, note}], archived [ids], warnings}, `show` {text}. Errors end with the CLI exit code: 1 conflict (`if_v`, tamper), 2 refused input (limits, secret-shaped values, unknown ids), 3 store unavailable. Never put a secret value in a field.

| Parameter | Type | Required |
|---|---|---|
| `brief` | `boolean` | no |
| `code` | `string` | no |
| `if_v` | `integer` | no |
| `op` | `string` | yes |
| `ops` | `array` | no |
| `project` | `string` | no |
| `request_id` | `string` | no |
| `session` | `string` | no |
| `store` | `object` | no |

## Sibling Daemon MCP Surfaces

Each sibling daemon owns its own tool catalog in its own crate — out of scope for direct extraction here. See that daemon's own descriptor source for its exact tool list:

- **trusty-search** — `crates/trusty-search/src/mcp/tools/descriptors.rs`
- **trusty-analyze** — `crates/trusty-analyze/src/mcp/descriptors.rs`
- **trusty-memory** — `crates/trusty-memory/src/mcp_service.rs`
