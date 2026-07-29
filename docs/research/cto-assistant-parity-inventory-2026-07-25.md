# Legacy "CTO Assistant" Slack Bot — Parity Inventory for trusty-agents Cutover (#3856)

**Date**: 2026-07-25
**Scope**: Full capability/knowledge/deployment inventory of a legacy Python Slack bot built for a private consulting client, for replacement by the trusty-agents `cto-assistant` agent.

> **REDACTED FOR PUBLIC RELEASE.** This document was written against a private
> client engagement and has been de-identified before landing in this public
> repository. Angle-bracket placeholders (`<client>`, `<checkout-prod>`,
> `<primary-db>`, `<user-1>`, …) stand in for real names, paths, hostnames,
> repository names, database/table names, account identifiers, and people.
> Organisation scale figures, financials, project code names, and Slack member
> IDs have been removed outright rather than masked. Everything of engineering
> value — the capability inventory, the parity gaps, and the cutover risks — is
> unchanged. Placeholders are consistent throughout: the same placeholder always
> refers to the same underlying thing.

## 0. Checkout notes (read this before trusting any path below)

There are at least three live clones of the client's private `cto` repository, plus unrelated stale dirs. All inventory below was read from the **freshest code**, cross-checked against the **actual production/data layout**:

| Checkout | Last commit | Role |
|---|---|---|
| `<checkout-dev>` | 2026-07-23 | **Freshest code** — used for all file/line citations below. Has 82 **broken** symlinks under `data/` (no sibling `cto-resources/`, so no real DB access from here) — a pure code checkout, not production-connected. |
| `<checkout-prod>` | 2026-07-14 | **Current production home** (pm2 `ecosystem.config.js` here has the migrated `cwd`, and pm2's saved dump references this path for the hiring app). Data symlinks resolve correctly against sibling `<checkout-prod>/../data/`. |
| `<checkout-legacy>` | 2026-07-13 | **Old/pre-migration production home**, superseded 2026-07-09. Still has old rotated `cto-bot` logs (last activity before 2026-07-13). A one-shot `reclaim-reminder` launchd job was scheduled to assess reclaiming this checkout but never produced a report (job didn't fire / was archived silently — no `reclaim-reminder-*.md` found). |
| Two further stale directories | n/a | Not git repos, not this bot. Ignored. |

**Real production data root**: the sibling `cto-resources/data/` directory (e.g. `<primary-db>` ≈ 33 MB, modified 2026-07-23). Both `<checkout-legacy>` and `<checkout-prod>` resolve their `data/*` symlinks against it; the development clone does not.

**Live-status finding (important for #3856)**: the interactive Slack bot process itself does not appear to be currently running. `pm2 list` is empty on this host; the saved pm2 dump (`~/.pm2/dump.pm2`) only contains the reporting and hiring apps, not `cto-bot`; and `<checkout-prod>/logs/` has no `cto-bot-*` log files at all (the only `cto-bot` logs on disk are the old, rotated ones under `<checkout-legacy>/logs/`, last touched before the 2026-07-09 migration). Meanwhile the **launchd cron fallback** (`<launchd-prefix>.hourly_sync`, running `scripts/cron/hourly_sync.sh` out of `<checkout-prod>`) is actively firing every hour through 2026-07-24 21:58 — but 3 of its 4 sub-jobs (email check, meeting sync, Slack sync) are **currently failing** with unhandled exceptions each run, stuck at a "last check" cursor of 2026-07-13 (i.e., broken since right around the migration). Only the Gmail-helper sub-pipeline succeeds. **Net effect: for roughly the last 10+ days, the legacy bot has answered no live Slack DMs, and three of its four background sync pipelines have been silently failing.** This lowers cutover risk (little live-state to migrate) but means "what the bot currently does in prod" and "what the code says it does" have diverged — treat the code (this inventory) as the spec, not recent bot behavior.

---

## 1. Tools / capabilities (Bedrock `toolSpec` entries)

All tools are `Service` implementations registered into a `ServiceRegistry` (`app/cto_bot/services/registry.py`, `app/cto_bot/services/base.py`) and exposed to Bedrock's `converse()` tool-use loop. Source of truth: `grep -n 'name="' app/cto_bot/services/*.py` in the freshest checkout — this list is **more complete than the bot's own `app/cto_bot/AGENT.md`** (dated 2026-05-15; 5 tools below post-date it and are undocumented there).

| Tool name | File | Tier | Restricted (tier) | Purpose |
|---|---|---|---|---|
| `get_tga_report` | `services/tga_service_wrapper.py:60` | on_demand | — | Git Flow Analytics report: velocity, quality, ai_usage, work_classification, pod_rollup, risks, individual, raw_scores. Params: `report_type` (required), `pod`, `name`, `weeks` (default 4). |
| `get_team_members` | `services/db_services.py:33` | on_demand | ANALYTICS blocked | List R&D team members: title, org, department, git commits, JIRA tickets, from `<primary-db>`. |
| `get_budget_data` | `services/db_services.py:71` | on_demand | ANALYTICS blocked | R&D budget: top cost centers, work-type allocation, from `<primary-db>`. |
| `query_cto_db` | `services/analytics_query_service.py:96` | on_demand | — (not in AGENT.md) | Read-only ad-hoc SQL (SELECT/WITH only, 500-row cap) against `<primary-db>` — budget, LLM-usage, and person/roster tables. |
| `query_analytics` | `services/analytics_query_service.py:20` | on_demand | — (not in AGENT.md) | Read-only ad-hoc SQL against `<analytics-db>` — a weekly-engineer fact table, DORA/pod/org rollup views, and recruiting-pipeline, forecast, and budget-by-initiative/product tables. |
| `generate_chart` | `services/analytics_query_service.py:175` | on_demand | — (not in AGENT.md) | Renders a PNG (bar/line/stacked_bar) and uploads to Slack via a `[CHART: ...]` token the LLM must echo back verbatim. |
| `search_email` | `services/email_service.py:32` | on_demand | ANALYTICS blocked | Recent priority-flagged Gmail messages w/ Gmail deep-links. Param: `limit` (default 10). |
| `sync_emails` | `services/email_sync_service.py:558` | background | — | Sync unread Gmail, priority-score, DM the owner a summary, create Google Tasks. Params: `since_hours` (2), `dry_run`. |
| `classify_emails` | `services/email_classifier_service.py:325` | on_demand | ANALYTICS blocked | Classify recent emails (actionable/fyi/newsletter/.../spam), flag reply-needed, auto-archive bulk noise. Params: `limit` (50), `use_llm` (true), `dry_run`. |
| `run_gmail_helper` | `services/gmail_helper_service.py:1866` | background | — (not in AGENT.md) | Orchestrates 5 Gmail sub-pipelines: GitHub-notification triage, weekly bulk-filter proposals, direct-email todos, content-store sync, expense/receipt capture. Params: `pipelines[]`, `dry_run`. |
| `list_open_tasks` | `services/task_service.py:422` | on_demand | — | List the owner's open Google Tasks. Params: `task_list`, `include_due` (true). |
| `complete_task` | `services/task_service.py:462` | on_demand | — | Mark a Google Task complete (id, id-prefix, or fuzzy title). Param: `task_id` (required), `task_list`. |
| `remind_open_tasks` | `services/task_service.py:500` | on_demand | — | Summarize overdue / due-within-N-hours tasks; optional Slack DM. Params: `hours_lookahead` (24), `send_slack`. |
| `search_meeting_notes` | `services/granola_service.py:33` | on_demand | — | Search Granola meeting notes/transcripts (via MCP). Params: `query` (required), `limit` (3), `scope` (notes/transcripts). |
| `sync_meeting_notes` | `services/meeting_sync_service.py:177` | background | — | Pull new Granola notes, write to `projects/meetings/YYYY-Wnn/`, create Tasks, DM summary. Params: `since_hours` (2), `dry_run`. |
| `get_calendar_events` | `services/calendar_service.py:42` | on_demand | — | Upcoming Google Calendar events. Params: `window` (today/this_week), `time_min`, `time_max`, `calendar_id` (primary). |
| `check_availability` | `services/calendar_service.py:101` | on_demand | — | Free/busy check. Params: `time_min`, `time_max` (required), `calendars[]` (primary). |
| `search_confluence_docs` | `services/confluence.py:96` | on_demand | — | Search local Confluence markdown dumps (5 `<confluence-space>` exports) under `systems/confluence/`. Param: `query` (required). |
| `search_codebase` | `services/vector_search_service.py:60` | on_demand | — | Semantic search over the `cto` project (code + markdown + Confluence dumps) via trusty-search. Params: `query` (required), `limit` (5). |
| `sync_slack_messages` | `services/slack_sync_service.py:215` | background | — (not in AGENT.md) | Collect new Slack channel messages bot is a member of, write daily transcripts to `systems/slack/transcripts/YYYY-Wnn/`, extract owner-assigned action items (Claude Haiku on Bedrock), create Google Tasks, DM summary. Params: `since` (ISO override), `dry_run`. |
| `get_train_schedule` | `services/<commuter-rail>_service.py:58` | on_demand | — | Upcoming `<commuter-rail>` departures between two stations. Params: `from_station`, `to_station` (required), `count` (5). |
| `get_train_alerts` | `services/<commuter-rail>_service.py:112` | on_demand | — | Active `<commuter-rail>` service alerts. Param: `line` (optional). |
| `core_memory` / `<client>_memory` | `services/context.py`, `services/mcp_client.py:187-195` | **always_on** (not a Bedrock tool the LLM calls — injected every turn) | — | trusty-memory palace recall/remember/learn (`cto` palace) + a live git-log snippet of recent `app/cto_bot/` commits ("RECENT BOT CHANGES", for self-referential "what's new" questions). |

**Stale/declared-but-unimplemented**: `app/cto_bot/service_config/services.yaml` still lists tier overrides for `get_product_portfolio` and `get_bus_factor_risks` — grepping the entire `services/` tree finds **no implementation** for either name. These are dead config, not real gaps.

**Special (non-LLM) Slack commands**, handled before Bedrock dispatch in `handlers/message.py`:

| Command | Behavior |
|---|---|
| `!clear` / "clear history" / "reset" | Clears conversation history |
| `!help` / "help" / "?" | Lists tier-appropriate capabilities |
| `!status` | Quick team/budget snapshot (`ContextService.team_snapshot()`) |
| `!memory` | Shows recent trusty-memory palace entries |
| `!canvas <path>` | Opens a local `.md` file as a Slack Canvas |
| `!bug` | Files a local bug ticket via `aitrackdown bug create` CLI (`services/bug_reporter.py`) |
| `!ghissue` | Creates a GitHub issue, routed by keyword heuristic to one of 5 private `<client-org>` repositories — labels `[bug, cto-reported]` (`services/github_issue_service.py`) |
| `!mpm` | Escalates to MPM SDK (allowlisted users only, default owner only) |

**LLM reply-embedded tokens** (post-processed by `FileHandlerService`/`CanvasSyncService`, not Bedrock tools):
`[CREATE_CANVAS: path.md]`, `[SAVE_CANVAS: path.md]...[/SAVE_CANVAS]`, `[MOVE_FILE: old → new]`, `[DELETE_FILE: path.md]`, `[READ_FILE: path.md]` — all constrained to `.md` files under `projects/`.

---

## 2. Slack surface

- **Connection**: Socket Mode (`slack_bolt.async_app.AsyncApp` + `AsyncSocketModeHandler`), not Events API HTTP. `app/cto_bot/main.py:161,170`.
- **App manifest**: `app/slack_app_manifest.yaml` / `.json`. App name "CTO Assistant". `socket_mode_enabled: true`, `org_deploy_enabled: false`, `interactivity: false`. Event subscription: `bot_events: [message.im]` only.
- **OAuth scopes**: `im:history`, `im:read`, `im:write`, `chat:write`, `reactions:write`, `reactions:read`, `users:read`, `users:read.email` (optional).
- **Handlers registered** (`main.py` composition root → `handlers/message.py`, `handlers/canvas.py`):
  - `message` — DMs, the primary interface; filtered to non-bot/non-system messages.
  - `app_mention` — registered but not the primary surface.
  - `canvas_updated` — triggers canvas → local `.md` sync-back.
- **Threading**: conversation history is per-user (DM), not per-thread; session persistence keyed by Slack user id (see §7).
- **Reactions**: `🤔` thinking-indicator reaction added/removed around tool-call processing (needs `reactions:write`/`reactions:read`).
- **Liveness**: a `SocketWatchdog` (`services/socket_watchdog.py`) tracks event staleness; if the WebSocket is silently dead >15 min (default `SOCKET_WATCHDOG_MINUTES`) it forces reconnect, and after repeated failures calls `os._exit(1)` so pm2 restarts the process.
- **Access gate**: allowlist-only. Non-allowlisted users are silently routed to a separate "Virtual CTO" persona (public-only, no tools, no internal data — see §4).

---

## 3. Knowledge sources

| Source | Location | Access method | Inside `cto` git repo? |
|---|---|---|---|
| `<primary-db>` (SQLite) | `data/` → symlink into sibling `cto-resources/data/` | `DatabaseService` (direct SQL) + `query_cto_db` tool | **NO** — gitignored, lives in sibling `cto-resources/` dir outside the repo |
| `<analytics-db>` (DuckDB) | `data/` (symlink) | `query_analytics` tool | **NO** — same as above |
| `<analytics-elt-db>`, `<analytics-public-db>` | `data/*.duckdb` (symlinks) | ETL/reporting apps, not directly bot tools | **NO** |
| `bot_session.db` (SQLite) | `data/bot_session.db` | `SharedSessionService` — conversation history | **NO** |
| `canvas.db` (SQLite) | `data/canvas.db` | `CanvasDB` — Slack Canvas state | **NO** |
| `gitflow_cache.db` | referenced by `GFAReportEngine` for `get_tga_report` | SQLite | Not confirmed in-repo; TGA-specific cache, likely also under `data/`/resources |
| Confluence markdown dumps | `systems/confluence/<confluence-space>/` | File search (`ConfluenceService`) + trusty-search | **YES** — `git ls-files systems/` returns several thousand tracked files |
| Gmail dump | `systems/gmail/messages.json`, `systems/gmail/classified/` | File read/write | **YES** — tracked |
| Slack transcripts | `systems/slack/transcripts/YYYY-Wnn/` | Written by `sync_slack_messages`, read via search | **YES** — tracked |
| Granola meeting notes/transcripts | Live via `granola-notes` MCP server; local copies in `projects/meetings/YYYY-Wnn/` | MCP tool calls + file search | Local copies likely tracked; live source is external (Granola cloud) |
| Google Workspace (Gmail, Calendar, Tasks) | Live via `gworkspace-mcp` MCP server | MCP tool calls | External (Google cloud), not in repo |
| trusty-search index | HTTP daemon `http://127.0.0.1:7878/indexes/cto/search`, CLI fallback | `search_codebase` tool | Indexes the repo (many thousands of chunks per AGENT.md; exclusions in `.trusty-search-exclusions.md`) |
| trusty-memory palace `cto` | Subprocess CLI (not MCP) | `TrustyMemoryService`, always-on context | External store (kuzu-backed), not repo files |
| JIRA (large issue corpus, per system prompt) | Referenced in system prompt as a live capability, but **no JIRA MCP server or service implementation found** in `services/` — likely surfaced only via indexed Confluence/JIRA export dumps under `systems/`, not a live query tool | n/a | Unclear — flag for verification |

**Critical parity gap to flag**: the new agent binds `okg://cto-assistant` + a trusty-search index **over the `cto` repo**. The repo-tracked knowledge (Confluence dumps, Gmail dumps, Slack transcripts, meeting-note copies under `systems/`/`projects/`) is coverable by that index. **The structured databases (`<primary-db>`, `<analytics-db>`, `bot_session.db`, `canvas.db`) are NOT in the repo at all** — they live in a sibling `cto-resources/` directory, are gitignored, and are queried via direct SQL (`query_cto_db`, `query_analytics`, `get_team_members`, `get_budget_data`), not semantic search. A trusty-search index of the repo will **not** surface this data, and semantic search is the wrong tool for it anyway (it's tabular/relational). Parity requires either (a) a SQL-query tool pointed at the same DB files, or (b) migrating/re-deriving that data into whatever the new agent's structured-data story is (OKG facts, a new DB, etc.).

---

## 4. Prompts / persona

Two full system prompts in `app/cto_bot/prompts.py` (196 lines total), selected per-user by allowlist membership.

### Primary persona — `SYSTEM_PROMPT` (allowlisted users)

Structurally load-bearing sections, paraphrased (the verbatim text names the client and two individuals and is not reproduced here):

> "You are the CTO Assistant for `<client>`, a private AI assistant available only to `<user-1>` (CTO) and `<user-2>` (Engineering Operations Coordinator)."

Contains **hardcoded company/org facts** — a codebase size estimate, an R&D headcount split (FTE vs contractors), an annual R&D spend figure, a key-people roster, a senior-leadership-team roster, and a list of key project code names. *(All specific figures, names, and project code names redacted; the parity-relevant point is only that this material is hardcoded into the prompt and would have to be re-authored for the new agent.)* Also carries a "Your Capabilities" section enumerating live data sources (`<primary-db>`, Confluence page corpus, JIRA issue corpus, Git across a large repo fleet, Granola, Gmail, Google Tasks, an SSO/identity provider, "Fact Finder" cross-source verification, bot changelog).

Notable behavioral directives worth preserving verbatim:
- **Scope — Answer Everything**: "you must NOT refuse or deflect general questions... Never say things like 'that's not in my wheelhouse'... Just answer." Includes an explicit carve-out that it must NOT redirect commuter-rail questions to external apps.
- **Response Style**: concise/direct, Slack markdown (`*bold*`, `_italic_`, `•` bullets — no markdown tables since Slack doesn't render them), cite source when citing data ("per `<primary-db>`"), "Never fabricate numbers — only cite data provided in your context", no Gmail links in replies (don't render in Slack).
- **Source Transparency (MANDATORY for generated documents)**: any generated table/matrix/compliance doc must append a `**Source notes:**` block distinguishing ✅ Confirmed (with source) vs ⚠️ Inferred rows, or an all-confirmed/all-inferred statement. Explicitly "non-negotiable for compliance, security, access-control, and audit documents."
- **Canvas / File Operation token grammar**: exact rules for `[CREATE_CANVAS:]`, `[SAVE_CANVAS:]...[/SAVE_CANVAS]`, `[MOVE_FILE:]`, `[DELETE_FILE:]`, `[READ_FILE:]`, restricted to `.md` under `projects/`.

### Fallback persona — `VIRTUAL_CTO_PROMPT` (non-allowlisted users)

> "You are the Virtual CTO — a public-facing AI assistant representing `<user-1>`, CTO of `<client>`..."

Explicit allow/deny topic lists: may discuss tech strategy, architecture patterns, AI/ML within the client's industry vertical, engineering culture/DevOps, OSS/cloud/Python/Java, career advice, published talks. **Must NOT discuss**: specific employees/contractors, comp/budget/financials, internal org/headcount, confidential projects/roadmap, customer names/contracts, security vulnerabilities/infra, internal tools/DB access. Standard decline line: *"I can't share internal details, but I'm happy to discuss the general technology approach."* Ends with "All conversations are logged for security purposes."

**Parity note**: this dual-persona / allowlist-gated content split is a distinct feature the new agent needs an equivalent for if it will ever be reachable by users outside the small allowlist — otherwise it can be dropped if the new agent is DM-only to the same 2-3 people.

---

## 5. Model / provider

- **Provider**: AWS Bedrock, via `boto3` (`services/bedrock.py`).
- **Model**: `us.anthropic.claude-sonnet-4-6` (env `BEDROCK_MODEL`, default in `config.py:213`); AGENT.md's header claims the same model — consistent.
- **Region**: `us-east-1` (env `AWS_REGION`).
- **Auth**: AWS credentials via a named profile (`~/.aws/config` has a matching `[profile …]` entry, `region = us-east-1`) — the weekly TGA launchd job explicitly sets `AWS_PROFILE` because launchd doesn't source shell rc files.
- **Streaming**: Not confirmed from files read so far — the `converse()` Bedrock API call pattern (tool loop: build messages → call → inspect `toolUse` blocks → re-call) as described in AGENT.md doesn't indicate `converseStream`; likely non-streaming request/response per turn, with a Slack "thinking" 🤔 reaction as the only progress indicator. (Not independently verified by reading `bedrock.py` line-by-line — flag for a follow-up check if streaming behavior specifically matters for parity.)
- A second, smaller model is used internally for cheap extraction: `sync_slack_messages`'s action-item extraction uses **Claude Haiku on Bedrock** (`slack_sync_service.py`, via `task_extractor.py`).

---

## 6. Hosting / deployment (feeds #3856 directly)

- **Process manager**: **pm2**, `ecosystem.config.js` (repo-tracked) defines 5 apps: a Flask web app, `cto-bot` (this Slack bot, Socket Mode — no HTTP port), a sanitized public analytics web app, a loopback-only hiring app handling sensitive compensation/PII data, and an `ai-analytics` cron-style pm2 job (hourly via `cron_restart: '0 * * * *'`). All web apps bind loopback-only ports.
- **Current/production checkout**: `<checkout-prod>` (migrated 2026-07-09 from `<checkout-legacy>`; see `ecosystem.config.js` `cwd` fields and the migration manifest referenced from `.env.local.example`: `projects/systems/operations/CTO-RESOURCES-MIGRATION-MANIFEST.md`). Data root: the sibling `cto-resources/data/`.
- **Bot launch script**: `app/start_slack_bot.sh` → `python -m app.cto_bot.main` (per `main.py` module docstring), venv at `.venv/bin`.
- **Cron fallback** (when the bot process is down): `launchd` agents under `~/Library/LaunchAgents/`:
  - `<launchd-prefix>.hourly_sync` → `scripts/cron/hourly_sync.sh` (runs every 3600s) — chains: email check (`hourly_email_check.py`), meeting sync (Granola), Slack sync, Gmail helper pipelines. **Currently 3/4 sub-jobs failing** (see §0).
  - `<launchd-prefix>.tga_weekly` → `<checkout-reports>/scripts/cron/tga_weekly.sh`, Mondays 05:00, uses a named `AWS_PROFILE` to reach `<report-bucket>` and ECS. Note this points at a **fourth** location (`<checkout-reports>`), a separate reporting-only checkout not otherwise covered in this inventory.
  - `<launchd-prefix>.daily_compliance` → `scripts/cron/daily_compliance_sync.sh`, daily 02:30, working dir `<checkout-legacy>` (still points at the **old, pre-migration** checkout — likely stale/needs its own migration check).
  - `<launchd-prefix>.reclaim-reminder` — one-shot (target 2026-07-16), meant to assess reclaiming the old `<checkout-legacy>` checkout; strictly read-only; no report was found to have been produced (job either never fired or silently archived without writing output — worth a human check if the old checkout is still safe to keep around).
- **Current live status**: bot process not running (no pm2 entry, no fresh logs) as of this investigation — see §0 for detail.
- **Env vars** (`app/cto_bot/config.py`, loaded from `.env.local` at repo root via `_load_env_local`, or process env):

| Variable | Purpose | Default |
|---|---|---|
| `SLACK_BOT_TOKEN` | Bot OAuth token (`xoxb-…`) | required |
| `SLACK_APP_TOKEN` | Socket Mode app-level token (`xapp-…`) | required |
| `AWS_REGION` | Bedrock region | `us-east-1` |
| `BEDROCK_MODEL` | Inference model id | `us.anthropic.claude-sonnet-4-6` |
| `BOT_ALLOWED_USERS` | `ID:Name:TIER,...` | hardcoded 4-user default (see §7) |
| `BOT_MPM_SDK_USERS` | Slack IDs allowed MPM escalation | owner's ID only |
| `BOT_MAX_HISTORY` | Conversation turn window | 40 |
| `BOT_COMPRESSION_THRESHOLD` | Compress after N turns | 30 |
| `TRUSTY_PALACE` | trusty-memory palace name | `cto` |
| `TRUSTY_SEARCH_URL` | trusty-search daemon URL | `http://127.0.0.1:7878` |
| `CTO_RESOURCES_DIR` | Override sibling data-resources root | `../cto-resources` (relative to repo) |
| `GMAIL_HELPER_ENABLED` | Master switch for hourly Gmail-helper loop | `false` |
| `GMAIL_GH_ISSUE_ENABLED` / `GMAIL_GH_ISSUE_DRYRUN` | GH-issue-from-email sub-feature gating | `true` / `true` |
| `GITHUB_TOKEN` | GitHub PAT for `!ghissue` and GH-notification triage | required for those features |
| Atlassian, Google OAuth, Notion, Datadog, and Salesforce credential vars | Other project-wide integrations (`.env.local.example`) — not all necessarily consumed by the bot itself; some are for adjacent ETL/report scripts in the same repo. | — |

- **MCP servers** the bot connects to (from `.mcp.json` hierarchy, non-fatal if unavailable): `gworkspace-mcp` (Gmail/Calendar/Tasks), `granola-notes` (meeting notes/transcripts). trusty-memory and trusty-search are **not** MCP servers here — trusty-memory is CLI-subprocess, trusty-search is HTTP+CLI-fallback.
- **Secrets storage**: `.env.local` locally (gitignored); `.env.local.example` documents required keys; comment states "All secrets are stored in GitHub Secrets for CI/CD use" — actual secret custody for the always-on production process is the local `.env.local` file plus the named AWS credential profile in `~/.aws/credentials` (not read, per read-only/secrets-safety scope of this task).

---

## 7. State (conversation history, caches, session data)

| State | Location | Purpose | Migration need |
|---|---|---|---|
| `data/bot_session.db` (SQLite) | `SharedSessionService` | Per-Slack-user conversation history, window = `BOT_MAX_HISTORY` (40 turns), compressed via Bedrock summary at `BOT_COMPRESSION_THRESHOLD` (30 turns) | **Droppable** — conversational context resets are normal for an assistant cutover; not carrying meaningful long-term knowledge (that lives in trusty-memory/DB instead) |
| `data/canvas.db` (SQLite) | `CanvasDB` | Tracks which local `.md` files are open as which Slack Canvases, for sync-back | **Droppable/needs reconstruction** if the new agent supports Canvas-equivalent editing; otherwise N/A |
| `data/.email_sync_state.json`, `.meeting_sync_state.json`, `.slack_sync_state.json`, `.gmail_helper_state.json`, `.tracker_crawl_state.json`, `.roster_rebuild_state.json`, `.overnight_cleanup_state.json`, `.task_reminder_state.json`, `.confluence_extract_state.json`, `.confluence_sync_state.json` | `cto-resources/data/*.json` (symlinked into repo `data/`) | Incremental-sync cursors (dedup keys, per-channel cursors, "last check" timestamps) shared between the live bot and the cron fallback scripts | Needed only if the new agent reimplements the same incremental sync pipelines against the same sources; otherwise droppable — a fresh full sync/backfill is simpler than migrating brittle cursor state (and several of these are already stuck/broken per §0) |
| `<primary-db>`, `<analytics-db>` | sibling `cto-resources/data/` | The actual structured business data (people, budget, commits, DORA metrics, recruiting) | **Must migrate or re-point** — this is real business data, not disposable cache. See §3 gap. |
| Access allowlist | `BOT_ALLOWED_USERS` env, defaults in `config.py:177-191` | Four hardcoded Slack users: `<user-1>` (CTO, ALL tier), `<user-2>` (Engineering Operations Coordinator, ALL), `<user-3>` (ALL), `<user-4>` (ANALYTICS). *(Slack member IDs and real names redacted.)* | Must be reproduced in the new agent's access control — see `app/cto_bot/ACCESS-RULES.md` for the governance doc (tier table, change log, annual recert note) |
| Chat logs | `logs/` (via `ChatLogger`) | Plain-text conversation logging | Droppable |

---

## PARITY CHECKLIST

| Legacy capability | trusty-agents equivalent | Status |
|---|---|---|
| `get_tga_report` (GFA velocity/quality/ai_usage/etc.) | tagent tool calling `gitflow_cache.db` / TGA engine, if wired up | **GAP** — needs a new tool; not part of default OKG/trusty-search bind |
| `get_team_members`, `get_budget_data`, `query_cto_db`, `query_analytics` | Structured DB query tool over `<primary-db>`/`<analytics-db>` | **GAP** — these are SQL tools, not semantic search; `okg://cto-assistant` + trusty-search index alone will not cover this (data isn't even in-repo, see §3) |
| `generate_chart` (PNG → Slack) | Chart-rendering + Slack-upload tool | **GAP** — no equivalent noted |
| `search_email`, `sync_emails`, `classify_emails`, `run_gmail_helper` | Gmail MCP tool + background pipeline | **GAP** — depends on whether trusty-agents has a Gmail/gworkspace MCP binding for this agent |
| `list_open_tasks`, `complete_task`, `remind_open_tasks` | Google Tasks MCP tool | **GAP** — same dependency as above |
| `search_meeting_notes`, `sync_meeting_notes` | Granola MCP tool | **GAP** — needs `granola-notes`-equivalent MCP server bound to the new agent |
| `get_calendar_events`, `check_availability` | Google Calendar MCP tool | **GAP** — same MCP dependency |
| `search_confluence_docs`, `search_codebase` | trusty-search over `cto-assistant` index | **COVERED** (for repo-tracked content) / **PARTIAL** (Confluence dumps are in-repo and indexable; live Confluence is not queried live by the legacy bot either, so parity is achievable) |
| `sync_slack_messages` | Slack-channel-history sync tool/listener | **GAP** — no mention of an equivalent background Slack-channel collector for trusty-agents |
| `get_train_schedule`, `get_train_alerts` (commuter rail) | — | **GAP**, low priority — trivially droppable "nice to have" per system prompt's "answer everything" ethos, not core to CTO duties |
| Always-on memory context (`core_memory`/`<client>_memory`) | `okg://cto-assistant` + trusty-memory palace | **COVERED** in spirit — the new agent's OKG binding is the direct architectural successor; content needs re-derivation since it's currently sourced from a different palace (`cto`) |
| System prompt / persona (SYSTEM_PROMPT, org facts, response style, Canvas grammar, source-transparency rule) | New agent's persona/system prompt | **GAP** — must be authored fresh; the "Source Transparency (MANDATORY)" rule and "Answer Everything" scope rule are behaviorally important and easy to silently drop |
| Virtual CTO fallback persona for non-allowlisted users | — | **GAP or N/A** — only needed if the new agent is reachable beyond the current 2-3 allowlisted humans |
| Slack DM Socket Mode surface, allowlist gating, tier system (ALL/ANALYTICS) | slack-mcp / trusty-agents Slack listener + access control | **PARTIAL** — need to confirm trusty-agents' Slack integration supports per-user tiered tool restriction, not just yes/no allowlisting |
| `!bug` (aitrackdown), `!ghissue` (GitHub issue routing across 5 repos) | tm-bug-reporting pipeline / GitHub tool | **PARTIAL** — trusty-mpm already has bug-reporting plumbing; the 5-repo keyword-routing logic for `!ghissue` would need porting if kept |
| `!mpm` MPM SDK escalation | N/A (trusty-agents IS the successor architecture) | **N/A / SUPERSEDED** |
| Canvas open/save/move/delete/read tokens | Slack Canvas tool, if trusty-agents has one | **GAP** — no evidence of an equivalent in what's been described of the new agent |
| Conversation history + compression (`bot_session.db`) | tagent's own session/history handling | **COVERED** by tagent's native session model (different mechanism, same purpose) |
| pm2 hosting, launchd cron fallback | trusty-mpm daemon/session model | **N/A / SUPERSEDED** — new agent's hosting model is presumably trusty-mpm-native, not pm2 |

---

## TOP RISKS FOR CUTOVER

1. **Structured DB data is outside the repo and outside semantic search's reach.** `<primary-db>`/`<analytics-db>` (team roster, budget, DORA metrics, recruiting pipeline) live in a gitignored sibling `cto-resources/` directory and are queried via raw SQL tools, not text search. Binding `okg://cto-assistant` + a trusty-search index **over the `cto` repo** will silently miss all of it unless a SQL-query tool (or an OKG ingestion pipeline) is built against the same DB files. This is very likely the single biggest functional gap — `get_team_members`/`get_budget_data`/`query_cto_db`/`query_analytics` together are core "CTO Assistant" value.
2. **Silent behavioral rules in the system prompt are easy to drop and hard to notice missing.** In particular: the "Source Transparency (MANDATORY)" block for generated tables/matrices/compliance docs, and the "Answer Everything — never deflect" scope rule. Both are explicit, opinionated behavior deliberately authored into the prompt; a generic new-agent prompt won't reproduce them unless copied deliberately.
3. **Access-tier model (ALL vs ANALYTICS) blocks specific tools per user**, not just gross allow/deny. If the new Slack surface only supports binary "can talk to the bot," the ANALYTICS-tier user (currently blocked from `get_team_members`/`get_budget_data`/email tools) either gets over-permissioned or under-permissioned unless the new agent reproduces per-tool tiering.
4. **Production is currently degraded, which could mask real dependencies during testing.** 3 of 4 hourly cron sync jobs (email, meeting, Slack) have been failing since ~2026-07-13 and the interactive bot process hasn't logged activity since before that. Anyone validating "does the old bot still do X" against the live system right now will get false negatives; validate against the **code**, not observed current behavior. Also worth a quick human check on why the crons broke — could be an MCP auth/token issue relevant to the new agent's own MCP bindings (`gworkspace-mcp`, `granola-notes`) if they'll be reused.
5. **Multiple stale/duplicate checkouts create real risk of inventorying or cutting over the wrong copy.** Production runs from `<checkout-prod>`; the freshest git history is in a completely different clone (`<checkout-dev>`) that can't even see the real data; a fourth checkout (`<checkout-reports>`) is a separate reporting-only project used by the weekly TGA cron. The `daily_compliance` launchd job still points at the **old, pre-migration** `<checkout-legacy>` path — worth flagging to the owner directly, independent of the trusty-agents cutover.
6. **JIRA is claimed in the system prompt but no live JIRA service/MCP binding was found in `services/`.** Likely served only via static indexed dumps under `systems/`. If the new agent's knowledge base doesn't include an equivalent JIRA export, this specific claim in the persona becomes false and should be either backed by real data or removed from the new prompt.
7. **Undocumented tools found only by reading code, not the bot's own docs.** `AGENT.md` (the bot's self-documentation, dated 2026-05-15) is missing `query_cto_db`, `query_analytics`, `generate_chart`, `run_gmail_helper`, `sync_slack_messages` entirely. Do not rely on `AGENT.md` alone for a parity sign-off — this inventory was built by cross-referencing it against `grep` over the actual `services/*.py` `ServiceSpec` declarations.

---

## Key file references

All paths are relative to the freshest checkout (`<checkout-dev>`).

- `app/cto_bot/AGENT.md` — bot's own (partially stale) self-documentation
- `app/cto_bot/ACCESS-RULES.md` — access tier governance doc
- `app/cto_bot/prompts.py` — both system prompts, verbatim
- `app/cto_bot/config.py` — env vars, allowlist/tier defaults, MCP config merge logic
- `app/cto_bot/main.py` — composition root, all service registrations, background loops, watchdog wiring
- `app/cto_bot/service_config/services.yaml` — tier overrides (includes 2 stale entries)
- `app/cto_bot/services/*.py` — one file per tool/service (see table in §1 for exact line numbers)
- `app/resources.py` — resolves `data/`/`backups/` to the sibling `cto-resources/` root; explains the broken-symlink situation in the development checkout
- `app/slack_app_manifest.yaml` — Slack app scopes/settings
- `ecosystem.config.js` — pm2 process definitions (5 apps)
- `.env.local.example` — required secrets/env inventory
- `~/Library/LaunchAgents/<launchd-prefix>.{hourly_sync,tga_weekly,daily_compliance,reclaim-reminder}.plist` — cron scheduling
- `<checkout-prod>/logs/hourly_sync_launchd.log` and the dated `hourly_sync_*.log` files — evidence of current cron failures
