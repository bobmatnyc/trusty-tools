# scripts

Every script resolves paths from its own location, so the checkout can live
anywhere. `TMUX_SOCKET` selects a tmux server; unset, tmux's own default
applies (`$TMUX` inside a session).

| Script | Purpose |
| --- | --- |
| `init.sh [--start] [--guard-lift]` | First-time setup, idempotent, the manual path beside `tm fleet init` (same tree): `inbox/`, `local/` (templates, ledgers, `.gitignore`), notify hooks for watch-set projects, settings key `statusLine` (only when absent), the guard lift `TRUSTY_MPM_PM_UNRESTRICTED=1` only with `--guard-lift` or `ARCHITECT_GUARD_LIFT=1` (the owner's choice, off by default), prints the `[supervisor]` grant line, runs `doctor.sh`; `--start` runs `architect-start.sh`. Test: `scripts/test_init.py`. |
| `doctor.sh [--no-host]` | Read-only post-install check; exits 1 on a miss. |
| `architect-start.sh [--rc]` | Post-launch shell step: poller at `--interval 600` (idempotent), `--rc` runs `rc-restore.sh tm-architect`. |
| `install-notify-hook.sh <project-dir> [--remove]` | Adds or removes the push hook in a watched project's `.claude/settings.json`. Idempotent. Stores the absolute path of `notify-supervisor.sh`, so re-run it after moving this checkout. |
| `notify-supervisor.sh` | The hook itself. Claude Code runs it on `permission_prompt`, `idle_prompt` and `agent_needs_input`. It appends one JSON line (`ts`, `project`, `tmux_session`, `tmux_pane`, `type`, `message`, `display`) to `inbox/events.jsonl` and always exits 0 quickly. Set `SUPERVISOR_INBOX_DIR` to write somewhere else, e.g. in tests. |
| `input-state.py <pane>...` | Classifies each pane's input box as `empty`, `suggestion` (Claude Code's dim auto-suggestion) or `typed`. Run it before sending anything to a pane. |
| `posted.sh <pane> <since HH:MMZ> [self-name]` | Double-post guard for a pane more than one supervisor watches. Lists submitted relay turns stamped at or after the given time, except the caller's own; exit 1 when any exist (do not send), exit 0 and `NONE` otherwise. The matching lives in `relay-marker.py`. |
| `relay-marker.py <since HH:MMZ> [self-name]` | Reads a pane capture on stdin and finds submitted relays. Accepts both markers, `[<Name> supervisor HH:MMZ]` and `[<Name> Architect HH:MMZ]`, typed after `❯` or delivered as `› Message from @…:`. A relay with no name never counts as the caller's own. Tests: `tests/test_relay_marker.py`. |
| `relay.sh <session> <text>` | The two-step send in one call. Refuses unless the box is `empty` or `suggestion`, and submits only when the typed box shows a relay marker (`supervisor` or `Architect`). |
| `session-relaunch.sh [--stop-tasks] <session> [log]` | Restarts one PM pane in place under the current Claude login: `/exit`, `tm` at the pane's shell, `/rename <session>`, `/remote-control <session>`, `/tm-session-resume`. Other sessions keep running. Logs to `inbox/relaunch.log`. Used after an account change (skill `tm-fleet-restart`). |
| `rc-restore.sh <session>...` | Re-enables Remote Control on live PM panes after an account change (status line `/rc failed`): per session `/rename <session>`, `tmux rename-window` to the session name, `/remote-control <session>` (the argument names the Remote Control session in the Claude app; `/rename` names only the local session). If RC is already active Claude shows a dialog: the script moves the selection to `Disconnect this session` (at most 3 Ups, else Escape and fail), waits for `Remote Control disconnected.`, and types `/remote-control <session>` again. It then accepts a `❯ Continue` prompt and confirms a `https://claude.ai/code/session_...` URL that differs from the one seen before. The shared steps live in `rc-lib.sh` (sourced by this script, `session-relaunch.sh` and `fleet-restart.sh`; test `scripts/test_rc_lib.py`). Each command is echo-verified (`input-state.py`) before a separate Enter. Skips a pane at a shell, busy (`esc to interrupt`) or holding typed input. Exit 0 all restored, 1 any failed, 2 only skips; one result line per session on stdout; steps log to `inbox/relaunch.log`. `fleet-poll.py` runs it automatically. |
| `fleet-restart.sh [--force] [--rc] <session>...` | Kills the tmux server and resumes the named sessions through the trusty-mpm daemon, then sends each `/tm-session-resume` (`--rc`: `/rename`, then `/remote-control <session>` with the disconnect path, first). Logs to `inbox/restart.log`. See below. |
| `usage.py summary \| plan \| sample \| ingest` | Claude account usage (the owner, 2026-10-07: plan cutovers). `summary`: per-account latest 5h and weekly %, reset times (UTC and America/New_York), reading age, source, and the current account's burn rate with the projected time to 100%. `plan`: whether a cutover is needed (limit reached, or projected before its reset), the time to switch, the best next account with reasons, weekly capacity left across accounts, hours of runway at the highest measured weekly burn, and the gap to the first weekly reset. A reading whose reset has passed counts as 0% and is marked `*` (inferred). `sample` records one reading from the statusline cache; `ingest` parses a `/usage` capture on stdin (`--source usage-cmd`). Ledger: `local/usage/ledger.jsonl` (`ts`, `account`, `pct5h`, `reset5h`, `pctwk`, `resetwk`, `weekly_models`, `source`, `note`; a `reset5h` of null means the window has not started). `fleet-poll.py` calls it each cycle and raises `usage` alerts. Test: `scripts/test_usage.py`. |
| `statusline-tee.sh` | The Architect's `statusLine.command` (`.claude/settings.json`). Caches Claude Code's `rate_limits` payload plus the logged-in account to `inbox/statusline-usage.json`, then runs `tm statusline` on the same input. A cache failure never breaks the status line. Source `statusline-cache`: no typing. |
| `usage-probe.sh <session> <pane> <account>` | Fallback source `usage-cmd`: types `/usage` into one idle pane (refuses unless `input-state.py` says `empty` or `suggestion` and no turn runs), captures, sends Escape, and ingests the figures. `fleet-poll.py` starts it at most once per hour, and at once after the logged-in account changes (up to 3 tries). |
| `dashboard_feed.py [--out PATH]` | Per-session agent and token usage for the dashboard (stdlib, no network, no artifact call). Reads the token watcher's outputs (`token_watch.report`, `inbox/token-watch-state.json`) and subagent transcript mtimes, maps Claude session ids to tmux names with the watcher's registry lookup, and writes `inbox/dashboard-feed.json` = `{sessions: {<tmux name>: {agents_active, agents_parked, out_tokens_1h, out_tokens_24h, units_24h, ctx_k, measured_at}}, fleet: {...}}`. Active: unfinished subagent written within 120 s; parked: unfinished, older. Fleet sums every session, including ones with no tmux name. `apply_feed()` (called by `fleet-poll.py` every cycle) writes only the changed `usage` fields of `sessions/<S>` docs and `meta/state.fleet_usage`, each pinned with `if_version`; a conflict skips the row until the next poll, and a failed apply raises one `dashboard` alert (no alert on success). Test: `scripts/test_dashboard_feed.py`, `scripts/test_fleet_poll_dashboard.py`. |
| `dashboard-serve.py [--port N]` | Localhost server for the dashboard (the owner, 2026-10-08 17:34Z; stdlib, binds 127.0.0.1 only, default port 7890, accepts Host/Origin 127.0.0.1/localhost plus this host's MagicDNS name (from `tailscale status --json`, plus repeatable `--allow-host NAME`; tailscale missing = localhost-only) so `tailscale serve --bg --https=7890 http://127.0.0.1:7890` works, refuses to start if the port is busy). Serves `local/artifacts/owner-queue/index.html` unchanged with a shim injected at serve time that provides `window.claude.use("db"\|"user")` over `/api` JSON endpoints (snapshots by 2 s polling). Documents are `local/owner-queue/<collection>/<id>.json` with an integer `version`; `if_version` mismatch returns 409; writes are atomic. Seeds `meta/state` (owner_name, next_ids from the `NEXT IDS` line) when absent. Start: `nohup python3 scripts/dashboard-serve.py >> inbox/dashboard-serve.log 2>&1 &`. Test: `scripts/test_dashboard_store.py`, `scripts/test_dashboard_discovery.py`, `scripts/test_dashboard_tailnet.py`. Writes `127.0.0.1:<port>` to `<data dir>/trusty-architect/http_addr` after bind (data dir: `$TRUSTY_DATA_DIR_OVERRIDE`, else `~/Library/Application Support` on macOS, else `${XDG_DATA_HOME:-~/.local/share}`) for trusty console discovery (trusty-tools #9474); removed on SIGTERM/SIGINT if still its own. |
| `dashboard-db.py get\|list\|set\|update\|delete\|batch` | The Architect's replacement for ArtifactData: reads and writes the same files through `dashboard_store.py` (flock, versions, atomic writes), so it works with the server down. `--json FILE_OR_STRING`, `--if-version N`, `list --status S`, `batch FILE` (all-or-nothing). JSON on stdout; exit 0 ok, 1 not found, 2 invalid, 3 version conflict. Test: `scripts/test_dashboard_store.py`. |
| `subagent-startup-tokens.py [dir]` | Prints count, median and max startup tokens per subagent type from the Architect transcripts (D34 runner measurement); test `test_subagent_startup_tokens.py`. |
| `verify-claim.py EVENT` | Checks the claims in one PM-message event (T07, D6b; stdlib, no model, read-only). `EVENT` is a file, `-` or literal JSON (schema below). Prints `MATCH <claim>`, `MISMATCH <claim>: expected X, got Y` or `UNCHECKED <claim>: <reason>` per claim. Exit 0 no mismatch, 1 any mismatch, 2 bad input. Runs only `gh pr view`, `gh pr checks`, `gh run view`, read-only git verbs and one crates.io GET (User-Agent `trusty-architect`); a guard refuses anything else. employer-org repos use `GH_CONFIG_DIR=$HOME/.config/gh-employer`, others the inherited default config, tokens unset. `VERIFY_CLAIM_OFFLINE=1` skips gh and curl. Test: `scripts/test_verify_claim.py`. |
| `eval-event-extractor.py [--model M]` | Scores the `event-extractor` agent (`.claude/agents/event-extractor.md`) on `scripts/fixtures/event-extractor.jsonl` (11 real PM messages with hand-written expected events). Runs `claude -p --agent event-extractor` per message in a scratch dir (live `inbox/events.jsonl` untouched), then reports accuracy for head, base, merge_sha, pr, version, kind, repo, SHAs invented, tokens per run. Bar: 100% on head/base/merge_sha/pr, 90% on kind; exit 0 met, 1 missed. `--score-only FILE` re-scores a saved `--out` file. Each run costs tokens. Test: `scripts/test_eval_event_extractor.py`. |

## PM message events (T07)

`event-extractor` (agent, `.claude/agents/event-extractor.md`) turns one PM
message into one line in `inbox/events.jsonl` and runs `verify-claim.py` on it.
The prompt is `FROM: <tmux session>` plus the message text (or `FILE: <path>`).
Event lines carry `kind`; the hook lines `notify-supervisor.sh` writes to the
same file carry `type`, and `fleet-poll.py` reads only `type`, so the two
shapes do not collide. The Architect keeps every decision, clearance, relay
and record write.

```
{ts, session, kind, repo, pr, branch, head, base, merge_sha, crate, version,
 ci_run, claims, asks, text_sha}
```

- `kind`: `clearance_request` `merged` `published` `paused` `status`
  `improvement` `question` `fyi`.
- `repo`: `owner/name` or a bare name (`trusty-tools`, `webapp`, `side-project`);
  the table in `verify-claim.py` maps it to the gh account and the local checkout.
- `pr`, `ci_run`: integers. `head`, `base`, `merge_sha`: copied from the message,
  matched by prefix of at least 10 hex characters. `crate`, `version`: set only
  when the message is about releasing or publishing that version.
- `claims` (all optional): `pr_state`, `merged_at`, `files`, `commits`,
  `checks {pass, fail, pending}`, `crates_io_max`, `ci_conclusion`, `ci_head`.
  With a PR number the PR claims run through gh; without one, `base`, `commits`
  and `files` run against the local checkout (`rev-list --count`, `diff --numstat`).
- `asks`: one line, what the PM wants from the Architect. `text_sha`: sha1 of the
  message text. Unknown values are null, never guessed.

## fleet-restart.sh

Use it only to replace a tmux server running at the wrong scheduling priority
(trusty-tools #8415). `kill-server` ends every session on the server.

Before running it:

1. Checkpoint every PM with `/tm-session-pause` and verify each snapshot file.
2. Checkpoint the supervisor the same way.
3. List every live session, including the supervisor's own:
   ```sh
   nohup scripts/fleet-restart.sh tm-api tm-web tm-architect &
   ```

It refuses to run, and logs why, when a live session is missing from the list
or when it cannot list the server's sessions. `--force` kills unlisted sessions
anyway; use it only after checkpointing them. Run it detached, because it kills
the terminal that started it.

## Testing without the fleet

Use a separate tmux server with a short socket path (tmux rejects paths over
~100 characters), and put a stub `tm` first on `PATH` so nothing reaches the
daemon:

```sh
T=$(mktemp -d /tmp/sv.XXXX); mkdir -p $T/bin
printf '#!/bin/sh\necho "stub tm $*"\n' > $T/bin/tm; chmod +x $T/bin/tm
tmux -S $T/s new-session -d -s a; tmux -S $T/s new-session -d -s b
PATH=$T/bin:$PATH TMUX_SOCKET=$T/s scripts/fleet-restart.sh a   # refuses: b unlisted
tmux -S $T/s kill-server; rm -rf $T
```

That example writes its log to this checkout's `inbox/restart.log`. To keep the
real log clean, copy `scripts/` into `$T` first and run the copy.
| `question-collector.py [--once]` | **Retired 2026-10-04 (f4).** Refuses to run (exit 2): its `collector-agent` is deleted. PMs now SendMessage the Architect and `fleet-poll.py` catches the rest. History stays in `local/question-events.jsonl`; `questions.py list` still reads it. |
| `start-question-collector.sh` | Retired with the collector (2026-10-04, f4); starts a session that exits at once. |
| `questions.py list \| answered \| relayed \| retire \| resolve \| history` | Folds the event log into the open queue and appends answered/relayed/retired events. |
| `qevents.py` | The event log (`local/question-events.jsonl`): event names, append under lock, fold. |
| `fleet-poll.py [--once] [--interval N] [--dry-run]` | Deterministic poller, no LLM. Every N s (default 60) it captures each tm-* PM pane and checks for an open dialog, a PM question, idle over 10 min, `ctx` at 22%+ (warn) / 25%+ (refresh; ruling b7 2026-10-07), plan-limit text or ⏳/📅 at 90%+, new error lines, and exited or vanished sessions. It also reads `permission_prompt`/`agent_needs_input` events, host load, disk and swap, and the supervisor's own context (`self_ctx`, below — the supervisor's pane is skipped from the per-pane checks, so this is the only source for its context). New alerts go to `inbox/alerts.jsonl`; dedup state is `inbox/poll-state.json`; one log line per cycle goes to `inbox/poll.log`. When a cycle has new alerts it types `[poll HH:MMZ] N new alerts -> <path>` into tm-architect, and only when that input box holds no typed text and no dialog. Remote Control: a PM pane whose status line shows `/rc failed` gets `rc-restore.sh <session>` (120 s timeout), at most once per session per 10 min (state in `poll-state.json` under `rc`), and an `rc_restored` (detail = URL) or `rc_failed` (detail = reason) alert. The Architect's own pane (`tm-architect`, otherwise skipped) gets the same through `architect_rc_step`: trigger is `/rc failed` or a `✻<account>` status-line field that differs from `architect_account` in `poll-state.json` (first sight only records it; the record moves after a successful restore); it acts only when `input-state.py` reports `empty` with no spinner, re-captures and re-checks immediately before running `rc-restore.sh tm-architect`, and types no `[poll …]` wake into the pane in a cycle where it ran (the wake is deferred to the next cycle). After a successful restore caused by an account change, every other tm-* PM gets `rc-restore.sh` in the same cycle (existing 10 min debounce). Test `scripts/test_fleet_poll_rc.py`. Each cycle it also renames any tm-* window whose name differs from its session to the session name (one `tmux list-windows -a`, then `tmux rename-window -t =<session>:<index>`; no keys; one `window-rename` line per rename in `poll.log`, no alert), because automatic-rename shows the Claude Code version. A busy pane or typed input is a skip, retried next poll; `rc_failed` alerts only after 3 consecutive skips. `--dry-run` runs one cycle and prints it: no files written, no keys sent. Architect watchdog (`architect-watchdog.py`, the owner's ruling f4, 2026-10-04): each cycle classifies tm-architect. **Dead** (session missing, or its pane at a shell): `architect_dead` alert, a macOS notification (osascript), and one relaunch via `session-relaunch.sh tm-architect` (creates the tmux session if missing; first prompt `/tm-session-resume`), at most one per 30 min; two failed relaunches give `architect_relaunch_failed` and stop until the Architect is seen alive. **Silent** (alive, a wake sent over `SILENT_MINUTES` (default 45) ago, no transcript write since): `architect_silent` alert and notification only; never a relaunch, never keys. **Frozen** (the pane shows a running turn AND the newest transcript is older than `FROZEN_MIN` (default 15 min) or the spinner text is byte-identical across two polls): `architect_frozen` alert (re-emitted every 30 min) and notification; never a relaunch, never keys; it needs no wake. The `poll.log` line ends with `architect=<state>`. `--dry-run` shows the decision without acting. Test `scripts/test_fleet_poll_watchdog.py`. Goal effort tunes (More/Less Effort, store `effort_tunes`, status new) raise `owner_effort` alerts. |
| `alert_triage.py` | Triage layer after dedup in `fleet-poll.py` (the owner 2026-10-09 20:06Z): each new alert is `wake`, `log` or `ask_haiku` by rules plus the holds in `local/triage-rules.json` (`holds[]`: `session` with or without `tm-`, `reason`, `until` UTC or `until_event`, `set_by`, `set_at`; read every cycle, no restart). Always wake: `owner_*`, `permission_prompt`, `dialog`, `question`, `exited`, `self_ctx`, `band`, `architect_*`, `ctx` at 25%+, token `ctx_main` at 250k+, `rc_failed` 3 in a row per session. Log: `rc_restored`, token `hit_low`/`miss_idle`/`miss_unexpected`/`ctx_sub`, `ctx` below the bar, `idle` on a session with an unexpired hold. The rest goes in ONE batched `claude -p --model claude-haiku-5-5` call per cycle (plan login; alerts, last 30 pane lines, hold entry; emails and tokens redacted) returning `[{id, investigate, reason}]`; bad JSON, timeout, non-zero exit or a missing verdict wakes (fail open); `TRIAGE_HAIKU=off` disables the call and wakes. Every alert gets a line in `inbox/triage.jsonl` (`ts, id, kind, session, decision, path, reason`); `alerts.jsonl` is unchanged. Wake text: `[poll HH:MMZ] N alerts need you (M logged) -> inbox/alerts.jsonl`. `alert_triage.py --replay N` runs the real gate over the last N alerts. Test `scripts/test_fleet_poll_triage.py`. |
| `architect-watchdog.py` | Pure decision logic of the watchdog (classify dead/frozen/silent/ok, debounce, give-up); imported by `fleet-poll.py`, no I/O. |
| `pending-answers.py [--root DIR]` | Lists dashboard items needing the Architect (answered, cancelled, won't do, feedback, owner commands, goal effort tunes as kind `effort`); exit 1 if any, 0 none, 2 store error. Reader: `owner_answers.py`. Test `scripts/test_fleet_poll_owner_answer.py`. |
| — `load` alert | Fires on the **5-minute** load average only (`os.getloadavg()[1]`), not the 1-minute figure — a brief build spike (e.g. 29.7 one-minute / 16.8 five-minute) must not wake the supervisor for nothing (2026-09-27 06:28Z+). Threshold is `LOAD_FACTOR` (default 1.5, env-overridable) times the core count, matching the supervisor's own "urgent work" action line (`architect.md`: load under ~1.5x cores). The detail string always shows all three averages (`load 1m/5m/15m (5m > factor x cores)`). Re-emitted on the same 1800s interval as every other host alert. |
| — `self_ctx` alert | The supervisor's own pane has no `ctx N%` status-line field, so its context is measured by tailing the newest **interactive** `*.jsonl` under `SELF_CTX_DIR` (default: this project's own Claude Code project directory) for the last assistant `message.usage`, summing `input_tokens + cache_read_input_tokens + cache_creation_input_tokens` against a `SELF_CTX_WINDOW`-token window (default 1,000,000; env-overridable) at `SELF_CTX_THRESHOLD` percent (default 25, ruling b7 2026-10-07; env-overridable). "Interactive" matters: the same directory fills with short-lived `claude -p --agent` transcripts from `question-collector.py` every few minutes (entrypoint `sdk-cli`), which can out-mtime the supervisor's own session (entrypoint `cli`) and must never be the one measured — `scripts/self-ctx.py` checks only the 20 newest-by-mtime files for the `cli` tag, newest first, so this stays cheap even when collector churn dominates. At or over threshold it alerts, re-emitted on the same 1800s interval as `ctx`. A missing or unreadable transcript logs and is skipped, never crashes the poller. |
| — quiet sessions (`scripts/quiet-sessions.py`) | `local/quiet-sessions.txt` — one session name per line, `#` starts a comment, blank lines ignored, re-read every cycle; a missing file means no session is quiet. For a listed session (e.g. one the owner has parked or paused) only its `idle` alert is dropped; `dialog`, `question`, `error`, `exited` and `ctx` still fire. Separately, and regardless of that list, a session named `tm-xtest-*`, `tm-qa*-*` or `tm-r[0-9]*` never raises `exited` — those are throwaway test fixtures, not real PMs. |
| `start-fleet-poll.sh [args]` | Starts the poller in tmux session `tm-architect-poll` if not already running; arguments pass through (e.g. `--interval 30`). |

## Retention

`inbox/` holds transient queues and logs only; durable records go to the
`local/` git repository or a memory palace (`architect.md`, "Repository and
data"). Every writer opens its file per write and every offset reader resets
when the file shrinks, so rotating by rename is safe while the scripts run:
`mv inbox/<file> inbox/<file>.1` (replacing an older `.1`). Rotate by hand
for now; nothing rotates automatically.

| File | Writer | Readers | Retention |
| --- | --- | --- | --- |
| `events.jsonl` | `notify-supervisor.sh` (push hook) | `fleet-poll.py`, `question-collector.py` (byte offsets), `tm-fleet-check` (by `ts`) | Rotate above 5 MB; keep one `.1`. |
| `alerts.jsonl` | `fleet-poll.py` | the Architect, on each `[poll …]` wake | Rotate above 5 MB; keep one `.1`. |
| `poll-state.json` | `fleet-poll.py` | `fleet-poll.py` (dedup, events offset) | Keep. Deleting it skips the event backlog and re-raises open alerts once. |
| `poll.log`, `collector.log` | `fleet-poll.py`, `question-collector.py` | the Architect, the collector Monitor (`tail -F` follows a rename) | Rotate above 5 MB; keep one `.1`. |
| `statusline-usage.json`, `usage-probe.log` | `statusline-tee.sh`, `usage-probe.sh` (via `fleet-poll.py`) | `usage.py` | Overwritten on each render; rotate the log above 1 MB. The ledger itself is durable (`local/usage/`, committed). |
| `relaunch.log`, `restart.log` | `session-relaunch.sh`, `fleet-restart.sh` | `tm-fleet-restart` after a restart | Keep until the post-restart check is done; rotate above 1 MB. |
| zero-byte markers (`*-marker*`, `*.tmp`) | one-off procedures | none | Delete once no procedure that made them is in flight. |
| `briefs` | — | — | Symlink to `../local/briefs` (durable, committed in `local/`). |
