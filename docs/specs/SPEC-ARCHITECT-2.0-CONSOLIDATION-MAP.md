# SPEC-ARCHITECT-2.0-CONSOLIDATION-MAP: Architect 2.0 script consolidation map

**Specification ID:** SPEC-ARCHITECT-2.0-CONSOLIDATION-MAP
**Status:** Draft
**Date:** 2026-11-21
**Refs:** MB1595 (Rust scheduler spike, scope 5)

Scope item 5 of the Rust scheduler spike (brief MB1595). It says what happens to every file in the Architect's `scripts/`, in what order, and which registry jobs touch which script. Source repo: the Architect repo (`scripts/` and `local/` paths below are repo-relative). Read-only research; no script was run.

## Answer

- `scripts/` holds 550 files, not counting `__pycache__` and `.pytest_cache`. That is 240 scripts and docs, 268 test files and 42 fixture or test-data files.
- 178 scripts become Rust modules, 46 stay scripts (a registry job or an on-demand tool), 16 are retired.
- One scheduler in `architect::scheduler` is step 0. It replaces `poll-supervise.sh`, `start-fleet-poll.sh` and the schedule half of `il-cron.py`. The poller loop is already in Rust.
- Most scripts become Rust modules because they decide, write state or type into panes. Scripts that fit a job are periodic, network-bound or prototype tools.

## How to read this

- `architect::x` means crate `trusty-architect`, module `x`. "(exists)" means the module is already in `crates/trusty-architect/src/`.
- Dispositions: **Rust module** (port; the Python file is deleted after the port passes its record-then-replay check), **job (stays a script)** (stays Python; the scheduler runs it as a job, or it is an on-demand tool with no job), **retire** (delete, with or without a replacement).
- A test, fixture or test-data file takes the disposition of the file it tests. Python tests of a retired or ported script are deleted when that script is.
- Sources: file docstrings, `scripts/README.md`, `local/rust-port.md`, `docs/specs/SPEC-ARCHITECT-RUST-PORT.md` (the port spec), `local/meta/README.md`, `local/meta/registry.json`, and `grep` of `fleet-poll.py` for what it loads. Not every file was read in full. A docstring and the README decided most rows. The "proposed" bindings below are the author's inference, not the registry's.

## Counts

| Disposition | Scripts and docs | Test files | Fixtures and test data | Total |
|---|---|---|---|---|
| Rust module | 178 | 229 | 41 | 448 |
| job (stays a script) | 46 | 32 | 1 | 79 |
| retire | 16 | 7 | 0 | 23 |
| Total | 240 | 268 | 42 | 550 |

## Port list for 2.0

The port spec gives nine slices (section 5). Its rule: data first, then loops that write to it, then senders, then pages. This list adds the scheduler and the registry jobs in front, because the spike made the scheduler the center. Slice numbers in brackets are the port spec's.

| Step | Work | Replaces | Depends on |
|---|---|---|---|
| 0 | Scheduler core in `architect::scheduler`: config loader (registry schema), `every` and UTC cron, jitter, catch-up, overlap, script and agent runners, state, `runs.jsonl`, lock file, liveness file, shadow mode | schedule half of `il-cron.py`; Rust half of `meta_registry.py` | none (this is the spike) |
| 1 | Shadow parity, then cutover of the wake duty (M01): the scheduler runs `poller::cycle` and the `fleet-poll-ledger.py` shim | `poll-supervise.sh`, `start-fleet-poll.sh` (kept as rollback until accepted) | 0, parity report |
| 2 | Registry jobs for the poller's subprocess steps and the IL cron builtins: `board-feed`, `share-sync`, `task-ingest`, `task-progress`, `track-gh`, `runtime-selftest`, `saas-poll`, `il-cron` | the per-cycle subprocess calls in `fleet-poll.py` | 0, 1 |
| 3 | Move the factory ACTIONS text into the registry prompts for M40-M4a; enable M0d for the morning brief | `factory_tasks.py`, `morning_brief.py` | 0 and the Architect's ruling on factory runs |
| 4 | Store verbs, then the loopback API on 7890 [slices 1-2]: `dashboard-db.py`, answers, questions, the `dashboard_*` route modules, `dashboard_mask.py` | the store and route scripts | none for 1; API needs the store verbs |
| 5 | Ledger [slice 3]: remaining `datg` verbs, archive, reminders, aggregate, `tm dbatg hook`, IL builtins as `ledger::cron` | `datg-hook.sh`, `il-archive.py`, `il_remind.py`, `il_dashboard_sync.py`, `fleet-poll-ledger.py` (issue #11540) | 4 |
| 6 | Relay, pm, answers, questions, notify and hooks [slice 4] | `relay.sh`, `pm_*`, `pending-*`, `notify-supervisor.sh`, `ilio*`, `dash-footer-stop.*` | 5 |
| 7 | Tokens, usage probe, statusline cache, governor, tracking [slice 7 and #9801 slice 3] | `token_watch.py`, `dashboard_feed.py`, `usage-probe.sh`, `effort-*`, `task_*`, `track_*` | 4; jobs M02, M03 from step 0 |
| 8 | Context cycle and restore [slice 6]: `ctx watch\|cycle`, `rc-*`, `session-relaunch.sh`, `fleet-restart.sh` | `ctx-*.py`, `self-clear.sh`, `rc-*.sh` | 5, 6, 1 |
| 9 | Delete the Python poller and its modules after one week of Rust parity [slice 5] | `fleet-poll.py` and the `fleet_poll_*` modules | 1, 5, 6, 7, 8 |
| 10 | Admin verbs and one-shot deletions [slice 9]: `tm architect init\|doctor\|start`; delete `cutover.sh`, `dashboard-migrate*.py`, `il_remind_migrate.py`, `question-collector.py` | the init, doctor and one-shot scripts | none |
| 11 | Console rendering and chat [slice 8]: the dashboard page, Live, Visualize, media, Secrets nav, dashboard chat | `dashboard-serve.py` and the view and chat modules | 4 |

Each ported script follows the port spec's removal gate: release installed, record-then-replay clean, callers repointed, one full pass, then delete. Recommendation: steps 0-5 are the 2.0 line; 6-11 are 2.1. The port spec names slices 1-5 as the must-have set. 2.0 is due 2026-11-27, six days out (open question 1).

## Which registry jobs call which scripts

`registry.json` has 47 jobs (the spike brief says 46): 34 enabled, 13 disabled. Two are scheduler-run (`meta-due`, `M08`). 7 are `external:*` (`M01`-`M07`; the scheduler only checks their output). 38 are `managed_by: meta_due`.

| Job | Runs how | Script it calls | Basis |
|---|---|---|---|
| `meta-due` | scheduler, every 10 m | `meta_due.py check`; it imports `meta_registry.py` and `meta_il.py` and sends through `relay.sh` | registry argv; contract section 3 |
| `M08` | scheduler, agent `il-goal`, daily 12:00Z | none (`claude -p`) | registry |
| `M01` poll cycle | external: poller | `poll-supervise.sh` runs `fleet-poll.py`; `start-fleet-poll.sh` starts it | `poll-supervise.sh` header |
| `M02` usage sample | external: poller | `usage.py` (and `usage-probe.sh` via the poller) | `fleet-poll.py` loads `usage.py` |
| `M03` token rollup | external: poller | `token_watch.py` | `fleet-poll.py` loads it |
| `M04` IL cron | external: poller | `il-cron.py`, which calls `datg.py`, `il-archive.py`, `ctx-watch.py` | `il-cron.py` header |
| `M05` heartbeat | external: session cron | none | registry |
| `M06`, `M07` | external: launchd | none in `scripts/` (mpm-utils host-maint-check; `tm supervisor`) | registry |
| `M0a` goal reconcile | meta_due | `goal_reconcile.py` | registry runner text |
| `M0u` | meta_due | `meta_due.py report` | registry runner text |
| `M0n`, `M0r`, `M0t`, `M0u`, `M0v`, `M0w`, `M10`, `M11` | meta_due, `stamp` artifact | `meta_due.py stamp <id>` | registry check type |
| `M40`-`M4a` (11 jobs, all disabled) | meta_due, PM owner | `factory_tasks.py` ACTIONS text, sent by `relay.sh` | registry runner text |
| `M09`, `M0b`-`M0m`, `M0p`-`M0z` except those above | meta_due | none named; a skill or agent does the work | registry |

Proposed bindings the registry does not state:

- `M0b` dream digest runs `dream_catchup.py`; the poller's `dream` step calls it today.
- `M0c` runs `spec_drift_check.py`; `M0w` runs `ledger_spec_check.py`.
- `M0d` replaces the poller alert from `morning_brief.py`.
- `M0y` names a "hygiene collector". No file in `scripts/` matches it.

New jobs the scheduler needs (not in the registry): `board-feed`, `share-sync`, `task-ingest`, `task-progress`, `track-gh`, `runtime-selftest`, `saas-poll`. The first six are `fleet-poll.py` subprocess steps today (`fleet-poll.py` lines 1347-1479). `dashboard_feed.py` also runs as a subprocess there, but it is a Rust module in this map because it writes the store.

## Findings against the port spec

1. `fleet-classify.py` is in the port spec's section 1.1 twice, once as "fold" and once as "retire". `crates/trusty-architect/src/classify/` exists, so I mark it Rust module (exists) and delete the Python file.
2. `chat_store.py` and `chat_thread.py` look like ILIO prototype files but eight dashboard-chat modules import them (`chat_auth`, `chat_btw`, `chat_delivery`, `chat_files`, `chat_thr`, `chat_md`, `chat_view`, `chat_turns`). Only `chat-serve.py` and `chat-channel.py` are retired.
3. The port spec tables do not list the chat, task, track, instructions, share, model, secondary-user, `ilio`, `brief_ids` and lifecycle modules. This map places them. Rows for the view modules were deferred to slice 8 in `local/rust-port.md`.
4. `fleet-poll-ledger.py` is a temporary shim, deleted with issue #11540. The Rust poller already starts it each cycle for ten ledger-bound steps. The scheduler's shadow job must account for it.

## Open items

1. Rust triage for no-rule alerts (Haiku path). `alert_triage.py` sends an alert no rule decides to one batched Haiku call; the Rust poller makes no Haiku call and wakes for those alerts (Architect ruling, MB1595). `trusty-architect-sched parity` leaves those wakes out of its verdict and match percentages and prints their count.

## Open questions for the Architect

1. Release scope. Is 2.0 steps 0-5 only, given 2026-11-27? The alternative puts steps 6 and 8 (pane senders, context clear) on the critical path.
2. `meta_due.py`. Does it stay a scheduled script for 2.0, or does the scheduler take its freshness and escalation logic now? Both write the IL fields, so only one may.
3. Poller subprocess steps. Run `board_feed.py`, `share_sync.py`, `task_ingest.py`, `task_progress.py`, `track_gh.py` and `runtime_selftest.py` as script jobs (the author's proposal), or port each as a poller step?
4. Factory runs. M40-M4a are disabled and `factory_tasks.py` is retired in this map. Enable them, or drop the concept?
5. Morning brief. After `morning_brief.py` retires, does the Architect still want a poller `morning_brief` wake, or only the M0d trigger?
6. Shared state during overlap. Which process owns `inbox/poll-state.json` and `inbox/il-cron-state.json` while Python and Rust both run?
7. `model_*.py`. Keep, or retire when the Fable 5.2 versus Opus 5.5 comparison ends? Nothing in `scripts/` schedules `model_autoscore.py daily`.
8. launchd jobs. `log-drain` stays outside the scheduler here. Should the scheduler adopt it after 2.0? The spike forbids touching existing launchd jobs now.
9. Registry count. The brief says 46 jobs; the file has 47. Which is right?
10. Retire confirmations: `chat-serve.py`, `chat-channel.py`, `il-settings.py`, `morning_brief.py`, `factory_tasks.py`. Nothing else in `scripts/` imports `chat-serve.py` or `chat-channel.py`, but callers were not checked outside `scripts/`.


## Full table: every script and doc file in scripts/

One row per file. 240 rows. Test, fixture and test-data files follow in the next section, grouped by target.

| file | disposition | target | reason |
|---|---|---|---|
| `README.md` | job (stays a script) | doc (no job) | Trimmed to the scripts that remain. |
| `alert_triage.py` | Rust module | architect::poller::triage (exists) | Haiku triage already ported to the poller. |
| `aliases.py` | Rust module | architect::aliases | Name resolver. |
| `architect-chat-serve.py` | Rust module | architect::mcp | Decided: architect-mcp moves to Rust; the Muse HTTP chat joins it. |
| `architect-idle.py` | Rust module | architect::input / verbs::{turn_active,idle} (input exists) | Pane-state checks used before every send. |
| `architect-mcp.py` | Rust module | architect::mcp | Decided: architect-mcp moves to Rust; the Muse HTTP chat joins it. |
| `architect-start.sh` | Rust module | tm architect init/doctor/start | Port spec N4. |
| `architect-watchdog.py` | Rust module | architect::poller::architect (exists) | Watchdog step already in the poller. |
| `architect_config.py` | Rust module | architect::config | Instance settings. |
| `auto_sleep.py` | Rust module | architect::governor | Port spec slice 3 of #9801; sends keys to panes. |
| `board_feed.py` | job (stays a script) | new job board-feed (proposed; poller subprocess step today) | Has its own cadence gate and uses gh; fits a script job. |
| `brief-id.py` | Rust module | architect::briefs::ids | Durable brief ids. |
| `brief_ids.py` | Rust module | architect::briefs::ids | Durable brief ids. |
| `browser_drive.py` | job (stays a script) | on demand (no job) | Prototype tool, not scheduled. |
| `build_context.py` | Rust module | console (Foundry) + architect::dashboard::views; post-2.0 | Rendering tier; port spec says the console takes it over. |
| `build_pipeline.py` | Rust module | console (Foundry) + architect::dashboard::views; post-2.0 | Rendering tier; port spec says the console takes it over. |
| `build_waits.py` | Rust module | console (Foundry) + architect::dashboard::views; post-2.0 | Rendering tier; port spec says the console takes it over. |
| `chat-channel.py` | retire | none | ILIO chat prototype, superseded by dashboard-serve --chat (README line 31). |
| `chat-serve.py` | retire | none | ILIO chat prototype, superseded by dashboard-serve --chat (README line 31). |
| `chat_activity.py` | Rust module | architect::chat (+ console view); post-2.0 | Dashboard chat over tmux; chat_store and chat_thread are still imported by it. |
| `chat_auth.py` | Rust module | architect::chat (+ console view); post-2.0 | Dashboard chat over tmux; chat_store and chat_thread are still imported by it. |
| `chat_bg.py` | Rust module | architect::chat (+ console view); post-2.0 | Dashboard chat over tmux; chat_store and chat_thread are still imported by it. |
| `chat_blobs.py` | Rust module | architect::chat (+ console view); post-2.0 | Dashboard chat over tmux; chat_store and chat_thread are still imported by it. |
| `chat_btw.py` | Rust module | architect::chat (+ console view); post-2.0 | Dashboard chat over tmux; chat_store and chat_thread are still imported by it. |
| `chat_decommission.py` | Rust module | architect::chat (+ console view); post-2.0 | Dashboard chat over tmux; chat_store and chat_thread are still imported by it. |
| `chat_delivery.py` | Rust module | architect::chat (+ console view); post-2.0 | Dashboard chat over tmux; chat_store and chat_thread are still imported by it. |
| `chat_files.py` | Rust module | architect::chat (+ console view); post-2.0 | Dashboard chat over tmux; chat_store and chat_thread are still imported by it. |
| `chat_md.py` | Rust module | architect::chat (+ console view); post-2.0 | Dashboard chat over tmux; chat_store and chat_thread are still imported by it. |
| `chat_model.py` | Rust module | architect::chat (+ console view); post-2.0 | Dashboard chat over tmux; chat_store and chat_thread are still imported by it. |
| `chat_store.py` | Rust module | architect::chat (+ console view); post-2.0 | Dashboard chat over tmux; chat_store and chat_thread are still imported by it. |
| `chat_task.py` | Rust module | architect::chat (+ console view); post-2.0 | Dashboard chat over tmux; chat_store and chat_thread are still imported by it. |
| `chat_thr.py` | Rust module | architect::chat (+ console view); post-2.0 | Dashboard chat over tmux; chat_store and chat_thread are still imported by it. |
| `chat_thread.py` | Rust module | architect::chat (+ console view); post-2.0 | Dashboard chat over tmux; chat_store and chat_thread are still imported by it. |
| `chat_tmux.py` | Rust module | architect::chat (+ console view); post-2.0 | Dashboard chat over tmux; chat_store and chat_thread are still imported by it. |
| `chat_tree.py` | Rust module | architect::chat (+ console view); post-2.0 | Dashboard chat over tmux; chat_store and chat_thread are still imported by it. |
| `chat_turns.py` | Rust module | architect::chat (+ console view); post-2.0 | Dashboard chat over tmux; chat_store and chat_thread are still imported by it. |
| `chat_view.py` | Rust module | architect::chat (+ console view); post-2.0 | Dashboard chat over tmux; chat_store and chat_thread are still imported by it. |
| `cli-profile.py` | job (stays a script) | on demand (no job) | Prototype tool, not scheduled. |
| `cli_profiles.py` | job (stays a script) | on demand (no job) | Prototype tool, not scheduled. |
| `commit_hooks.py` | Rust module | architect::track::commit_hooks | Git hooks over tasks.db. |
| `conftest.py` | job (stays a script) | test aid (no job) | Test sandbox for the Python that stays. |
| `ctx-cycle.py` | Rust module | architect::ctx | Clear signals and gated /clear; highest risk. |
| `ctx-watch.py` | Rust module | architect::ctx | Clear signals and gated /clear; highest risk. |
| `ctx_bar.py` | Rust module | architect::ctx | Clear signals and gated /clear; highest risk. |
| `cutover.sh` | retire | none | One-off move of the Architect from OLD to NEW. |
| `dash-footer-stop.py` | Rust module | architect::hooks::dash_footer | Stop hook. |
| `dash-footer-stop.sh` | Rust module | architect::hooks::dash_footer | Stop hook. |
| `dashboard-db.py` | Rust module | architect::dashboard::* (exists) | Store, encoding, schema and derive are in Rust (#10063). |
| `dashboard-migrate-validate.py` | retire | none | One-shot backfill or cutover tool. |
| `dashboard-migrate.py` | retire | none | One-shot backfill or cutover tool. |
| `dashboard-serve.py` | Rust module | console (Foundry) + architect::dashboard::views; post-2.0 | Rendering tier; port spec says the console takes it over. |
| `dashboard_add.py` | Rust module | architect::dashboard::rpc / http listener | API route handlers; slice 2. |
| `dashboard_alerts.py` | Rust module | architect::dashboard::rpc / http listener | API route handlers; slice 2. |
| `dashboard_archctx.py` | Rust module | console (Foundry) + architect::dashboard::views; post-2.0 | Rendering tier; port spec says the console takes it over. |
| `dashboard_attach.py` | Rust module | architect::dashboard::rpc / http listener | API route handlers; slice 2. |
| `dashboard_build.py` | Rust module | console (Foundry) + architect::dashboard::views; post-2.0 | Rendering tier; port spec says the console takes it over. |
| `dashboard_cancel.py` | Rust module | architect::dashboard::rpc / http listener | API route handlers; slice 2. |
| `dashboard_chat.py` | Rust module | architect::chat (+ console view); post-2.0 | Dashboard chat over tmux; chat_store and chat_thread are still imported by it. |
| `dashboard_derive.py` | Rust module | architect::dashboard::* (exists) | Store, encoding, schema and derive are in Rust (#10063). |
| `dashboard_docs.py` | Rust module | architect::dashboard::rpc / http listener | API route handlers; slice 2. |
| `dashboard_events.py` | Rust module | architect::dashboard::rpc / http listener | API route handlers; slice 2. |
| `dashboard_feed.py` | Rust module | architect::tokens::{watch,feed}; job M03 | Hourly rollup and per-session feed; port spec slice 7. |
| `dashboard_fleet.py` | Rust module | console (Foundry) + architect::dashboard::views; post-2.0 | Rendering tier; port spec says the console takes it over. |
| `dashboard_harness.py` | Rust module | console (Foundry) + architect::dashboard::views; post-2.0 | Rendering tier; port spec says the console takes it over. |
| `dashboard_harness_base.py` | Rust module | console (Foundry) + architect::dashboard::views; post-2.0 | Rendering tier; port spec says the console takes it over. |
| `dashboard_http.py` | Rust module | architect::dashboard::rpc / http listener | API route handlers; slice 2. |
| `dashboard_ids.py` | Rust module | architect::dashboard::* (exists) | Store, encoding, schema and derive are in Rust (#10063). |
| `dashboard_il.py` | Rust module | architect::ledger::aggregate / dashboard::il_sync | IL aggregate and sync into dashboard items. |
| `dashboard_jsonl.py` | Rust module | architect::dashboard::* (exists) | Store, encoding, schema and derive are in Rust (#10063). |
| `dashboard_lifecycle.py` | Rust module | architect::dashboard::rpc / http listener | API route handlers; slice 2. |
| `dashboard_mask.py` | Rust module | architect::dashboard::pane | Pane tail masking guards secrets; must port with the API. |
| `dashboard_media.py` | Rust module | console (Foundry) + architect::dashboard::views; post-2.0 | Rendering tier; port spec says the console takes it over. |
| `dashboard_page.py` | Rust module | console (Foundry) + architect::dashboard::views; post-2.0 | Rendering tier; port spec says the console takes it over. |
| `dashboard_pane.py` | Rust module | architect::dashboard::pane | Pane tail masking guards secrets; must port with the API. |
| `dashboard_pane_tail.py` | Rust module | architect::dashboard::pane | Pane tail masking guards secrets; must port with the API. |
| `dashboard_projects.py` | Rust module | architect::lifecycle | Projects nav and archive/restore; README says ports to trusty-architect. |
| `dashboard_qual.py` | Rust module | architect::dashboard::* (exists) | Store, encoding, schema and derive are in Rust (#10063). |
| `dashboard_remind.py` | Rust module | architect::dashboard::rpc / http listener | API route handlers; slice 2. |
| `dashboard_routes.py` | Rust module | architect::dashboard::rpc / http listener | API route handlers; slice 2. |
| `dashboard_schema.py` | Rust module | architect::dashboard::* (exists) | Store, encoding, schema and derive are in Rust (#10063). |
| `dashboard_secrets.py` | Rust module | console secrets nav; post-2.0 | Rendering and routes; the nav already talks to the console. |
| `dashboard_secrets_paste.py` | Rust module | console secrets nav; post-2.0 | Rendering and routes; the nav already talks to the console. |
| `dashboard_share.py` | Rust module | architect::dashboard::rpc / http listener | API route handlers; slice 2. |
| `dashboard_store.py` | Rust module | architect::dashboard::* (exists) | Store, encoding, schema and derive are in Rust (#10063). |
| `dashboard_tasks.py` | Rust module | console (Foundry) + architect::dashboard::views; post-2.0 | Rendering tier; port spec says the console takes it over. |
| `dashboard_updates.py` | Rust module | architect::dashboard::updates | Ingests task snippets and steering into items. |
| `dashboard_viz.py` | Rust module | console (Foundry) + architect::dashboard::views; post-2.0 | Rendering tier; port spec says the console takes it over. |
| `dashboard_watch.py` | Rust module | architect::lifecycle | Projects nav and archive/restore; README says ports to trusty-architect. |
| `datg-hook.sh` | Rust module | tm dbatg hook (architect::ledger) | Hook body becomes a verb. |
| `datg.py` | Rust module | architect::ledger::* (exists) | Intent Ledger store and verbs are in Rust (#9995). |
| `datg_guards.py` | Rust module | architect::ledger::* (exists) | Intent Ledger store and verbs are in Rust (#9995). |
| `datg_log.py` | Rust module | architect::ledger::* (exists) | Intent Ledger store and verbs are in Rust (#9995). |
| `datg_ops.py` | Rust module | architect::ledger::* (exists) | Intent Ledger store and verbs are in Rust (#9995). |
| `datg_read.py` | Rust module | architect::ledger::* (exists) | Intent Ledger store and verbs are in Rust (#9995). |
| `datg_ready.py` | Rust module | architect::ledger::* (exists) | Intent Ledger store and verbs are in Rust (#9995). |
| `datg_store.py` | Rust module | architect::ledger::* (exists) | Intent Ledger store and verbs are in Rust (#9995). |
| `datg_task.py` | Rust module | architect::ledger::* (exists) | Intent Ledger store and verbs are in Rust (#9995). |
| `db-ro.py` | job (stays a script) | on demand (no job) | Credential tools; port spec keeps them out of the 2.0 line. |
| `doc_sync.py` | job (stays a script) | on demand (no job) | Host-local or prototype tool; port spec keeps it Python. |
| `doctor.sh` | Rust module | tm architect init/doctor/start | Port spec N4. |
| `dream_catchup.py` | job (stays a script) | registry job M0b (daily 11:30Z) | Daily digest script; the registry job runs it and checks the digest file. |
| `effort-governor.py` | Rust module | architect::governor | Port spec slice 3 of #9801; sends keys to panes. |
| `effort-send.sh` | Rust module | architect::governor | Port spec slice 3 of #9801; sends keys to panes. |
| `eval-event-extractor.py` | job (stays a script) | on demand (no job) | Evaluation tool; port spec keeps it Python. |
| `factory_tasks.py` | retire | registry jobs M40-M4a (disabled) | Factory schedule moves to registry entries; retire once the ACTIONS text is the job prompt. |
| `fleet-classify.py` | Rust module | architect::classify (exists) | Pane parsing and alert rules are in Rust; delete the Python copy. |
| `fleet-poll-ledger.py` | retire | none (deleted with #11540) | Temporary shim for the ten ledger-bound steps; dies with the ledger port. |
| `fleet-poll.py` | Rust module | architect::poller::cycle (exists); job M01 | Poller loop is already in Rust; the scheduler runs the cycle as job M01. |
| `fleet-reload.py` | Rust module | architect::restore | Pane restart and Remote Control steps. |
| `fleet-restart.sh` | Rust module | architect::restore | Pane restart and Remote Control steps. |
| `fleet_alert_dedupe.py` | Rust module | architect::classify::dedup (exists) | Host-alert dedupe already in classify. |
| `fleet_poll_alerts.py` | Rust module | architect::poller::steps / ::host (exists) | Per-cycle alert and tmux/log reads are poller steps. |
| `fleet_poll_cfg.py` | Rust module | architect::poller::config (exists) | Env settings already in poller config. |
| `fleet_poll_io.py` | Rust module | architect::poller::steps / ::host (exists) | Per-cycle alert and tmux/log reads are poller steps. |
| `fleet_poll_selfrefresh.py` | Rust module | architect::poller::self_ctx_alerts (exists) | Self-refresh trigger already in Rust. |
| `fleet_poll_wake.py` | Rust module | architect::poller::wake (exists) | The wake pointer is already in Rust. |
| `fleet_poll_watchdog.py` | Rust module | architect::poller::architect (exists) | Poller watchdog step is in Rust. |
| `gh-session-track.py` | Rust module | architect::track::gh_sessions | Port spec slice 7. |
| `goal_reconcile.py` | job (stays a script) | registry job M0a (daily 11:23Z) | Deterministic daily script; replaces the 06:23 session cron. |
| `google_oauth.py` | job (stays a script) | on demand (no job) | Credential tools; port spec keeps them out of the 2.0 line. |
| `ids36.py` | Rust module | architect::ledger::* (exists) | Intent Ledger store and verbs are in Rust (#9995). |
| `il-archive.py` | Rust module | architect::ledger::archive | Port spec slice 3. |
| `il-cron.py` | Rust module | architect::scheduler + architect::ledger::cron; job M04 | Cadence jobs become registry entries; builtin prune/rotate/size-check/pilot-rescore become ledger ops. |
| `il-settings.py` | retire | none (hook wiring in tm architect init) | Prints a settings diff once; init replaces it. |
| `il_agent.py` | Rust module | architect::ledger::{agent,timeline,remind} | IL readers and reminders. |
| `il_dashboard_sync.py` | Rust module | architect::ledger::aggregate / dashboard::il_sync | IL aggregate and sync into dashboard items. |
| `il_remind.py` | Rust module | architect::ledger::{agent,timeline,remind} | IL readers and reminders. |
| `il_remind_migrate.py` | retire | none | One-shot migration of Remind tasks. |
| `il_timeline.py` | Rust module | architect::ledger::{agent,timeline,remind} | IL readers and reminders. |
| `ilio-start.py` | Rust module | architect::hooks::ilio | IL response model hooks; not in the port spec. |
| `ilio-start.sh` | Rust module | architect::hooks::ilio | IL response model hooks; not in the port spec. |
| `ilio-stop.py` | Rust module | architect::hooks::ilio | IL response model hooks; not in the port spec. |
| `ilio-stop.sh` | Rust module | architect::hooks::ilio | IL response model hooks; not in the port spec. |
| `ilio.py` | Rust module | architect::hooks::ilio | IL response model hooks; not in the port spec. |
| `init.sh` | Rust module | tm architect init/doctor/start | Port spec N4. |
| `input-state.py` | Rust module | architect::input / verbs::{turn_active,idle} (input exists) | Pane-state checks used before every send. |
| `install-codex.sh` | job (stays a script) | on demand (no job) | Host-local or prototype tool; port spec keeps it Python. |
| `install-commit-hooks.py` | Rust module | architect::track::commit_hooks | Git hooks over tasks.db. |
| `install-notify-hook.sh` | Rust module | architect::notify (hook body); tm project init | Port spec N4. |
| `install-tccd-guard.sh` | job (stays a script) | on demand (no job) | Host-local or prototype tool; port spec keeps it Python. |
| `instructions.py` | Rust module | architect::instructions | Dashboard Instructions section; prototype. |
| `instructions_adr.py` | Rust module | architect::instructions | Dashboard Instructions section; prototype. |
| `instructions_intake.py` | Rust module | architect::instructions | Dashboard Instructions section; prototype. |
| `instructions_verify.py` | Rust module | architect::instructions | Dashboard Instructions section; prototype. |
| `ledger_spec_check.py` | job (stays a script) | registry job M0w (weekly drift check) | Drift guard over spec invariants; reads docs, no state. |
| `level_lint.py` | Rust module | architect::level | DOC-91 technical level; port once settled. |
| `log-drain.py` | job (stays a script) | launchd com.architect.log-drain (outside the scheduler) | Host-local log copy to S3; stays a separate agent per the port spec. |
| `mcp_oauth_probe.py` | job (stays a script) | on demand (no job) | Credential tools; port spec keeps them out of the 2.0 line. |
| `meta_due.py` | job (stays a script) | registry job meta-due (every 10m) | Stays the first scheduled script; the scheduler runs it. Port after 2.0 if its logic should move into the scheduler. |
| `meta_il.py` | job (stays a script) | registry job meta-due (library) | Writes the System-maintenance IL fields for meta_due.py. |
| `meta_registry.py` | Rust module | architect::scheduler::{config,schedule,freshness} | Registry lint, cadence math and freshness tests are scheduler core; Python copy stays while meta_due.py runs. |
| `model_autoscore.py` | job (stays a script) | on demand; model_autoscore.py daily is a job candidate | Model comparison; not in the port spec. |
| `model_cases.py` | job (stays a script) | on demand; model_autoscore.py daily is a job candidate | Model comparison; not in the port spec. |
| `model_cases_report.py` | job (stays a script) | on demand; model_autoscore.py daily is a job candidate | Model comparison; not in the port spec. |
| `model_compare.py` | job (stays a script) | on demand; model_autoscore.py daily is a job candidate | Model comparison; not in the port spec. |
| `model_replay.py` | job (stays a script) | on demand; model_autoscore.py daily is a job candidate | Model comparison; not in the port spec. |
| `morning_brief.py` | retire | registry job M0d (weekdays 12:30Z) | Its schedule now lives in the registry; the alert step goes away when M0d runs. |
| `notify-supervisor.sh` | Rust module | architect::notify (hook body); tm project init | Port spec N4. |
| `notion-mcp.sh` | job (stays a script) | on demand (no job) | Host-local or prototype tool; port spec keeps it Python. |
| `notion_sync.py` | job (stays a script) | on demand (no job) | Host-local or prototype tool; port spec keeps it Python. |
| `owner_answers.py` | Rust module | architect::answers | Answer hook and its dedupe. |
| `pending-answers-hook.sh` | Rust module | architect::answers | Answer hook and its dedupe. |
| `pending-answers.py` | Rust module | architect::answers | Answer hook and its dedupe. |
| `pending_hook_dedupe.py` | Rust module | architect::answers | Answer hook and its dedupe. |
| `pm-digest.py` | Rust module | architect::pm | PM message intake and cursor; port spec slice 3. |
| `pm-notify.sh` | Rust module | architect::pm | PM message intake and cursor; port spec slice 3. |
| `pm_cursor.py` | Rust module | architect::pm | PM message intake and cursor; port spec slice 3. |
| `pm_intake.py` | Rust module | architect::pm | PM message intake and cursor; port spec slice 3. |
| `poll-reset-wake.py` | retire | none | One-shot reset of a stuck wake backlog; not a recurring tool. |
| `poll-supervise.sh` | retire | architect::scheduler (supervision, lock, liveness file) | Scheduler daemon replaces the hang watchdog; keep as the rollback path until cutover is accepted. |
| `posted.sh` | Rust module | architect::relay | Two-step guarded send and the owner-citation guard; one module. |
| `project_lifecycle.py` | Rust module | architect::lifecycle | Projects nav and archive/restore; README says ports to trusty-architect. |
| `project_lifecycle_ports.py` | Rust module | architect::lifecycle | Projects nav and archive/restore; README says ports to trusty-architect. |
| `provision-app-key.sh` | job (stays a script) | on demand (no job) | Credential tools; port spec keeps them out of the 2.0 line. |
| `qevents.py` | Rust module | architect::questions | Owner-question event log and launcher. |
| `question-collector.py` | retire | none | Already exits 2 since 2026-10-04 (f4). |
| `questions.py` | Rust module | architect::questions | Owner-question event log and launcher. |
| `questions_rc.py` | Rust module | architect::questions | Owner-question event log and launcher. |
| `quiet-sessions.py` | Rust module | architect::quiet (exists) | Already ported. |
| `rc-disconnect.sh` | Rust module | architect::restore | Pane restart and Remote Control steps. |
| `rc-lib.sh` | Rust module | architect::restore | Pane restart and Remote Control steps. |
| `rc-restore.sh` | Rust module | architect::restore | Pane restart and Remote Control steps. |
| `rc_urls.py` | Rust module | architect::restore | Pane restart and Remote Control steps. |
| `relaunch_brief.py` | Rust module | architect::restore | Pane restart and Remote Control steps. |
| `relay-marker.py` | Rust module | architect::relay | Two-step guarded send and the owner-citation guard; one module. |
| `relay.sh` | Rust module | architect::relay | Two-step guarded send and the owner-citation guard; one module. |
| `relay_provenance.py` | Rust module | architect::relay | Two-step guarded send and the owner-citation guard; one module. |
| `relay_supersession.py` | Rust module | architect::relay | Two-step guarded send and the owner-citation guard; one module. |
| `ruling_pack.py` | job (stays a script) | on demand (no job) | Prototype tool, not scheduled. |
| `runtime_selftest.py` | job (stays a script) | new job runtime-selftest (proposed; poller subprocess step today) | Checks the live system for failures; runs as a job, rewritten per port as Python dies. |
| `saas-poll.py` | job (stays a script) | new job saas-poll (proposed) | Network poll of vendor balances; plain script, no state in Rust. |
| `sandbox_root.py` | job (stays a script) | test aid (no job) | Test sandbox for the Python that stays. |
| `secondary-guard.py` | Rust module | architect::guards::secondary | Rust target already named: tm hook --secondary-guard. |
| `secondary-guard.sh` | Rust module | architect::guards::secondary | Rust target already named: tm hook --secondary-guard. |
| `secondary_audit.py` | Rust module | architect::guards::secondary | Rust target already named: tm hook --secondary-guard. |
| `secondary_users.py` | Rust module | architect::guards::secondary | Rust target already named: tm hook --secondary-guard. |
| `secret-run.py` | job (stays a script) | on demand (no job) | Credential tools; port spec keeps them out of the 2.0 line. |
| `secrets_bridge.py` | job (stays a script) | on demand (no job) | Credential tools; port spec keeps them out of the 2.0 line. |
| `secrets_console.py` | job (stays a script) | on demand (no job) | Credential tools; port spec keeps them out of the 2.0 line. |
| `self-clear.sh` | Rust module | architect::ctx | Clear signals and gated /clear; highest risk. |
| `self-ctx.py` | Rust module | architect::self_ctx (exists) | Already ported. |
| `session-relaunch.sh` | Rust module | architect::restore | Pane restart and Remote Control steps. |
| `share_gdoc.py` | job (stays a script) | new job share-sync (proposed) | Google Docs sync prototype; network I/O stays a script job. |
| `share_links.py` | job (stays a script) | new job share-sync (proposed) | Google Docs sync prototype; network I/O stays a script job. |
| `share_requests.py` | job (stays a script) | new job share-sync (proposed) | Google Docs sync prototype; network I/O stays a script job. |
| `share_sync.py` | job (stays a script) | new job share-sync (proposed) | Google Docs sync prototype; network I/O stays a script job. |
| `spec_drift_check.py` | job (stays a script) | registry job M0c (daily drift check) | Anti-drift check of SPEC-ARCHITECT-COMPONENTS. |
| `stale-tasks.py` | Rust module | architect::dashboard::stale (exists in pending/stale.rs) | Stale-task list is in the dashboard crate. |
| `stalled_tasks.py` | Rust module | architect::hooks::stalled | UserPromptSubmit STALLED line. |
| `start-architect-questions.sh` | Rust module | architect::questions | Owner-question event log and launcher. |
| `start-fleet-poll.sh` | retire | architect::scheduler (supervision, lock, liveness file) | Scheduler daemon replaces the hang watchdog; keep as the rollback path until cutover is accepted. |
| `start-question-collector.sh` | retire | none | Already exits 2 since 2026-10-04 (f4). |
| `state-archive.py` | Rust module | architect::state_archive | Port spec slice 7. |
| `statusline-tee.sh` | Rust module | architect::usage::{probe,statusline} | Per the port spec; the probe types into panes. |
| `subagent-startup-tokens.py` | job (stays a script) | on demand (no job) | Evaluation tool; port spec keeps it Python. |
| `task_backfill_fields.py` | retire | none | One-shot backfill or cutover tool. |
| `task_budget.py` | Rust module | architect::track::* | Task tracking over tasks.db (SPEC-TRACK-01). |
| `task_events.py` | Rust module | architect::track::* | Task tracking over tasks.db (SPEC-TRACK-01). |
| `task_ingest.py` | job (stays a script) | new jobs task-ingest, task-progress, track-gh (proposed) | Poller subprocess steps today; each runs on its own cadence as a script job. |
| `task_movement.py` | Rust module | architect::track::* | Task tracking over tasks.db (SPEC-TRACK-01). |
| `task_progress.py` | job (stays a script) | new jobs task-ingest, task-progress, track-gh (proposed) | Poller subprocess steps today; each runs on its own cadence as a script job. |
| `task_pulse.py` | job (stays a script) | on demand; candidate job task-pulse | README says not wired into the poller or any cron. |
| `task_pulse_io.py` | job (stays a script) | on demand; candidate job task-pulse | README says not wired into the poller or any cron. |
| `task_reconcile.py` | Rust module | architect::track::* | Task tracking over tasks.db (SPEC-TRACK-01). |
| `task_steer.py` | Rust module | architect::dashboard::updates | Ingests task snippets and steering into items. |
| `task_tag.py` | Rust module | architect::track::* | Task tracking over tasks.db (SPEC-TRACK-01). |
| `task_tokens.py` | Rust module | architect::track::* | Task tracking over tasks.db (SPEC-TRACK-01). |
| `tccd-deadlock-guard.sh` | job (stays a script) | on demand (no job) | Host-local or prototype tool; port spec keeps it Python. |
| `technical_level.py` | Rust module | architect::level | DOC-91 technical level; port once settled. |
| `token_watch.py` | Rust module | architect::tokens::{watch,feed}; job M03 | Hourly rollup and per-session feed; port spec slice 7. |
| `track_events.py` | Rust module | architect::track::* | Task tracking over tasks.db (SPEC-TRACK-01). |
| `track_gh.py` | job (stays a script) | new jobs task-ingest, task-progress, track-gh (proposed) | Poller subprocess steps today; each runs on its own cadence as a script job. |
| `turn-active.py` | Rust module | architect::input / verbs::{turn_active,idle} (input exists) | Pane-state checks used before every send. |
| `usage-probe.sh` | Rust module | architect::usage::{probe,statusline} | Per the port spec; the probe types into panes. |
| `usage.py` | Rust module | architect::poller::steps::usage (exists); job M02 | Usage ledger and band already ported (#9801 slice 1). |
| `user_state.py` | Rust module | architect::governor | Port spec slice 3 of #9801; sends keys to panes. |
| `verify-claim.py` | Rust module | architect::verify | Port spec slice 7. |
| `verify_claim_input.py` | Rust module | architect::verify | Port spec slice 7. |
| `viz_cache.py` | Rust module | console (Foundry) + architect::dashboard::views; post-2.0 | Rendering tier; port spec says the console takes it over. |
| `watch-apply.py` | Rust module | architect::lifecycle | Projects nav and archive/restore; README says ports to trusty-architect. |

## Test, fixture and test-data files

310 files. Each takes the disposition of the file it tests. They are grouped by disposition and target; every file is named.

| disposition | target | count | files |
|---|---|---|---|
| Rust module | architect::aliases | 1 | `test_aliases.py` |
| Rust module | architect::answers | 1 | `test_pending_answers_hook.py` |
| Rust module | architect::briefs::ids | 1 | `test_brief_id.py` |
| Rust module | architect::chat (+ console view); post-2.0 | 26 | `test_chat_activity.py`, `test_chat_bg.py`, `test_chat_blobs.py`, `test_chat_btw.py`, `test_chat_clear.py`, `test_chat_decommission.py`, `test_chat_delivery.py`, `test_chat_md.py`, `test_chat_model.py`, `test_chat_task.py`, `test_chat_thread.py`, `test_chat_tmux.py`, `test_chat_tree.py`, `test_chat_turns.py`, `test_dashboard_chat.py`, `test_dashboard_chat_align.py`, `test_dashboard_chat_composer.py`, `test_dashboard_chat_main.py`, `test_dashboard_chat_sessions.py`, `test_dashboard_chat_side.py`, `test_dashboard_chat_thr.py`, `test_dashboard_chat_weekly.py`, `testdata/btw_answered.txt`, `testdata/btw_long.txt`, `testdata/btw_long2.txt`, `testdata/btw_pending.txt` |
| Rust module | architect::ctx | 4 | `test_ctx_bar.py`, `test_ctx_cycle.py`, `test_ctx_watch.py`, `test_self_clear.py` |
| Rust module | architect::dashboard::* (exists) | 28 | `fixtures/dashboard/store/accounts/acme.json`, `fixtures/dashboard/store/goal_removals/4c1ef80da1564f2ea1fe.json`, `fixtures/dashboard/store/goal_reorders/a7d6f0ab97fb4b86a890.json`, `fixtures/dashboard/store/goals/G01.json`, `fixtures/dashboard/store/goals/G02.json`, `fixtures/dashboard/store/goals/G03.json`, `fixtures/dashboard/store/goals/G04.json`, `fixtures/dashboard/store/items/A02.json`, `fixtures/dashboard/store/items/D01.json`, `fixtures/dashboard/store/items/D02.json`, `fixtures/dashboard/store/items/D03.json`, `fixtures/dashboard/store/items/D04.json`, `fixtures/dashboard/store/items/D05.json`, `fixtures/dashboard/store/items/T01.json`, `fixtures/dashboard/store/items/T02.json`, `fixtures/dashboard/store/meta/seen.json`, `fixtures/dashboard/store/meta/settings.json`, `fixtures/dashboard/store/meta/state.json`, `fixtures/dashboard/store/sessions/S01.json`, `fixtures/dashboard/store/sessions/S02.json`, `fixtures/dashboard/store/step_progress/A02.json`, `test_dashboard_derive.py`, `test_dashboard_ids.py`, `test_dashboard_jsonl.py`, `test_dashboard_schema.py`, `test_dashboard_store.py`, `test_priority.py`, `test_task_required_fields.py` |
| Rust module | architect::dashboard::pane | 2 | `test_dashboard_pane.py`, `test_dashboard_pane_filter.py` |
| Rust module | architect::dashboard::rpc / http listener | 6 | `test_dashboard_add.py`, `test_dashboard_alerts.py`, `test_dashboard_attach.py`, `test_dashboard_cancel.py`, `test_dashboard_events.py`, `test_dashboard_share.py` |
| Rust module | architect::dashboard::stale (exists in pending/stale.rs) | 1 | `test_stale_tasks.py` |
| Rust module | architect::dashboard::updates | 2 | `test_dashboard_updates.py`, `test_task_steer.py` |
| Rust module | architect::governor | 4 | `test_auto_sleep.py`, `test_effort_governor.py`, `test_user_state.py`, `test_user_state_effective.py` |
| Rust module | architect::guards::secondary | 2 | `test_secondary_guard.py`, `test_secondary_users.py` |
| Rust module | architect::hooks::dash_footer | 1 | `test_dash_footer_stop.py` |
| Rust module | architect::hooks::ilio | 2 | `test_ilio.py`, `test_ilio_isolation.py` |
| Rust module | architect::hooks::stalled | 1 | `test_stalled_tasks.py` |
| Rust module | architect::input / verbs::{turn_active,idle} (input exists) | 6 | `test_architect_idle.py`, `test_input_state_busy.py`, `testdata/input_busy_empty.raw`, `testdata/input_idle_empty.raw`, `testdata/input_idle_suggestion.raw`, `testdata/input_waiting_agents_empty.raw` |
| Rust module | architect::instructions | 3 | `test_instructions.py`, `test_instructions_adr.py`, `test_instructions_intake.py` |
| Rust module | architect::ledger::* (exists) | 2 | `test_datg.py`, `test_ids36.py` |
| Rust module | architect::ledger::aggregate / dashboard::il_sync | 3 | `test_dashboard_il.py`, `test_dashboard_il_steer.py`, `test_il_dashboard_sync.py` |
| Rust module | architect::ledger::archive | 1 | `test_il_archive.py` |
| Rust module | architect::ledger::{agent,timeline,remind} | 3 | `test_il_agent.py`, `test_il_remind.py`, `test_il_remind_ack.py` |
| Rust module | architect::level | 1 | `test_technical_level.py` |
| Rust module | architect::lifecycle | 2 | `test_dashboard_projects.py`, `test_project_lifecycle.py` |
| Rust module | architect::mcp | 2 | `test_architect_chat.py`, `test_architect_mcp.py` |
| Rust module | architect::pm | 3 | `test_pm_cursor.py`, `test_pm_digest.py`, `test_pm_intake.py` |
| Rust module | architect::poller::architect (exists) | 1 | `test_fleet_poll_watchdog.py` |
| Rust module | architect::poller::cycle (exists); job M01 | 14 | `test_fleet_poll_band.py`, `test_fleet_poll_ctx.py`, `test_fleet_poll_d22.py`, `test_fleet_poll_dashboard.py`, `test_fleet_poll_exited.py`, `test_fleet_poll_idle.py`, `test_fleet_poll_morning_brief.py`, `test_fleet_poll_nudge.py`, `test_fleet_poll_owner_answer.py`, `test_fleet_poll_rc.py`, `test_fleet_poll_stale.py`, `test_fleet_poll_triage.py`, `test_fleet_poll_windows.py`, `test_fleet_poll_zone.py` |
| Rust module | architect::poller::steps::usage (exists); job M02 | 1 | `test_usage.py` |
| Rust module | architect::questions | 2 | `test_architect_questions_launcher.py`, `test_questions_rc.py` |
| Rust module | architect::relay | 1 | `test_relay_sh_marker.py` |
| Rust module | architect::restore | 4 | `test_fleet_reload.py`, `test_rc_lib.py`, `test_relaunch_brief.py`, `test_session_relaunch_il.py` |
| Rust module | architect::scheduler + architect::ledger::cron; job M04 | 1 | `test_il_cron.py` |
| Rust module | architect::state_archive | 1 | `test_state_archive.py` |
| Rust module | architect::tokens::{watch,feed}; job M03 | 2 | `test_dashboard_feed.py`, `test_token_watch.py` |
| Rust module | architect::track::* | 6 | `test_task_budget.py`, `test_task_movement.py`, `test_task_reconcile.py`, `test_task_tag.py`, `test_task_tokens.py`, `test_track_events.py` |
| Rust module | architect::track::commit_hooks | 1 | `test_commit_hooks.py` |
| Rust module | architect::track::gh_sessions | 1 | `test_gh_session_track.py` |
| Rust module | architect::verify | 1 | `test_verify_claim.py` |
| Rust module | console (Foundry) + architect::dashboard::views; post-2.0 | 111 | `test_build_pipeline.py`, `test_build_waits.py`, `test_dashboard_3pane.py`, `test_dashboard_act_tools.py`, `test_dashboard_action_layout.py`, `test_dashboard_answer_overlay.py`, `test_dashboard_answer_processing.py`, `test_dashboard_ask.py`, `test_dashboard_autocollapse.py`, `test_dashboard_beep.py`, `test_dashboard_blocker_prefix.py`, `test_dashboard_brief_field_link.py`, `test_dashboard_brief_header_id.py`, `test_dashboard_brief_pending.py`, `test_dashboard_brief_refs.py`, `test_dashboard_brief_registry.py`, `test_dashboard_build.py`, `test_dashboard_building_brief.py`, `test_dashboard_busy.py`, `test_dashboard_clarify.py`, `test_dashboard_code_fence.py`, `test_dashboard_concurrency.py`, `test_dashboard_copy_block.py`, `test_dashboard_copy_path.py`, `test_dashboard_counters.py`, `test_dashboard_create_defaults.py`, `test_dashboard_crosslink.py`, `test_dashboard_decision_order.py`, `test_dashboard_discovery.py`, `test_dashboard_doc.py`, `test_dashboard_doc_links.py`, `test_dashboard_factory_goal.py`, `test_dashboard_feedback.py`, `test_dashboard_files.py`, `test_dashboard_footer.py`, `test_dashboard_footer_hook.py`, `test_dashboard_goal_done.py`, `test_dashboard_goal_kind.py`, `test_dashboard_goal_park.py`, `test_dashboard_goalprogress.py`, `test_dashboard_harness.py`, `test_dashboard_header_cache.py`, `test_dashboard_id_links.py`, `test_dashboard_in_progress.py`, `test_dashboard_input_hold.py`, `test_dashboard_instructions.py`, `test_dashboard_interactive.py`, `test_dashboard_item_brief.py`, `test_dashboard_item_project.py`, `test_dashboard_linkify_urls.py`, `test_dashboard_links_block.py`, `test_dashboard_live_progress.py`, `test_dashboard_localtime.py`, `test_dashboard_media.py`, `test_dashboard_media_archive.py`, `test_dashboard_media_cache.py`, `test_dashboard_media_controls.py`, `test_dashboard_media_ids.py`, `test_dashboard_meters.py`, `test_dashboard_mobile.py`, `test_dashboard_mode.py`, `test_dashboard_progress.py`, `test_dashboard_progress_at.py`, `test_dashboard_pruned.py`, `test_dashboard_qualified.py`, `test_dashboard_questions.py`, `test_dashboard_recommended.py`, `test_dashboard_reload.py`, `test_dashboard_reminder.py`, `test_dashboard_reminders.py`, `test_dashboard_requests.py`, `test_dashboard_reset_clock.py`, `test_dashboard_resume_steer.py`, `test_dashboard_run.py`, `test_dashboard_seen.py`, `test_dashboard_selftest_hidden.py`, `test_dashboard_session_cmd.py`, `test_dashboard_sessionbars.py`, `test_dashboard_settings.py`, `test_dashboard_specs_links.py`, `test_dashboard_status.py`, `test_dashboard_status_briefs.py`, `test_dashboard_status_update.py`, `test_dashboard_subs.py`, `test_dashboard_summary.py`, `test_dashboard_summary_wrap.py`, `test_dashboard_tailnet.py`, `test_dashboard_tally.py`, `test_dashboard_task_chat_link.py`, `test_dashboard_task_complete.py`, `test_dashboard_task_layout.py`, `test_dashboard_task_link.py`, `test_dashboard_task_links.py`, `test_dashboard_task_log.py`, `test_dashboard_task_notes.py`, `test_dashboard_task_order.py`, `test_dashboard_task_owner.py`, `test_dashboard_task_progress.py`, `test_dashboard_task_session_filter.py`, `test_dashboard_task_session_icon.py`, `test_dashboard_task_stars.py`, `test_dashboard_task_tabs.py`, `test_dashboard_task_tokens_row.py`, `test_dashboard_tasks.py`, `test_dashboard_tokens.py`, `test_dashboard_undo.py`, `test_dashboard_viz.py`, `test_dashboard_viz_cache.py`, `test_dashboard_wontdo.py`, `tests/busy_harness.js`, `tests/shim_harness.js` |
| Rust module | console secrets nav; post-2.0 | 4 | `test_dashboard_secrets.py`, `test_dashboard_secrets_parse.py`, `test_dashboard_secrets_paste.py`, `test_dashboard_secrets_picker.py` |
| Rust module | crates/trusty-architect golden corpus (record-then-replay) | 10 | `fixtures/dashboard/golden/SHA256SUMS`, `fixtures/dashboard/golden/accounts.jsonl`, `fixtures/dashboard/golden/derive.json`, `fixtures/dashboard/golden/goal_removals.jsonl`, `fixtures/dashboard/golden/goal_reorders.jsonl`, `fixtures/dashboard/golden/goals.jsonl`, `fixtures/dashboard/golden/items.jsonl`, `fixtures/dashboard/golden/meta.jsonl`, `fixtures/dashboard/golden/sessions.jsonl`, `fixtures/dashboard/golden/step_progress.jsonl` |
| Rust module | tm architect init/doctor/start | 1 | `test_init.py` |
| job (stays a script) | launchd com.architect.log-drain (outside the scheduler) | 1 | `test_log_drain.py` |
| job (stays a script) | new job board-feed (proposed; poller subprocess step today) | 1 | `test_board_feed.py` |
| job (stays a script) | new job runtime-selftest (proposed; poller subprocess step today) | 1 | `test_runtime_selftest.py` |
| job (stays a script) | new job saas-poll (proposed) | 1 | `test_saas_poll.py` |
| job (stays a script) | new job share-sync (proposed) | 2 | `test_share_requests.py`, `test_share_sync.py` |
| job (stays a script) | new jobs task-ingest, task-progress, track-gh (proposed) | 2 | `test_task_ingest.py`, `test_task_progress.py` |
| job (stays a script) | on demand (no job) | 13 | `fixtures/event-extractor.jsonl`, `test_cli_profile.py`, `test_db_ro.py`, `test_doc_sync.py`, `test_eval_event_extractor.py`, `test_google_oauth.py`, `test_notion_sync.py`, `test_ruling_pack.py`, `test_secret_run.py`, `test_secrets_bridge.py`, `test_secrets_console.py`, `test_subagent_startup_tokens.py`, `tests/test_browser_drive.py` |
| job (stays a script) | on demand; candidate job task-pulse | 1 | `test_task_pulse.py` |
| job (stays a script) | on demand; model_autoscore.py daily is a job candidate | 3 | `test_model_cases.py`, `test_model_compare.py`, `test_model_replay.py` |
| job (stays a script) | registry job M0a (daily 11:23Z) | 1 | `test_goal_reconcile.py` |
| job (stays a script) | registry job M0b (daily 11:30Z) | 1 | `test_dream_catchup.py` |
| job (stays a script) | registry job M0c (daily drift check) | 1 | `test_spec_drift_check.py` |
| job (stays a script) | registry job M0w (weekly drift check) | 2 | `test_ledger_spec.py`, `test_ledger_spec_check.py` |
| job (stays a script) | registry job meta-due (every 10m) | 1 | `test_meta_due.py` |
| job (stays a script) | test aid (no job) | 2 | `test_sandbox_root.py`, `test_store_live_guard.py` |
| retire | architect::scheduler (supervision, lock, liveness file) | 1 | `test_poll_supervise.py` |
| retire | none | 4 | `test_chat_channel.py`, `test_chat_serve.py`, `test_dashboard_migrate.py`, `test_dashboard_migrate_validate.py` |
| retire | none (deleted with #11540) | 1 | `test_fleet_poll_ledger.py` |
| retire | registry jobs M40-M4a (disabled) | 1 | `test_factory_tasks.py` |
