# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.4.3] — 2026-11-25

### Added

- `trusty-architect-sched parity --by-kind` prints matched / scheduler-only / poller-only per kind for alerts and wakes; `--examples N` lists up to N unmatched rows per kind (timestamp, session, fingerprint) (#11826).

### Fixed

- The scheduler's shadow reseeds its token-watch state from the live one at every shadow daemon start, under the live watcher's lock, and fails closed (keeping its own state) when the live file cannot be read or parsed, so a restart after downtime no longer replays old token alerts (#11826).
- The Rust watchdog applies the background-agent wait rule: an Architect waiting on background agents is not `architect_frozen` until the wait has sat unchanged for 60 minutes (#11826).
- `parity` leaves out `meta_due`, `meta_miss` and `factory_send_maxed`, which a dry-run poll never raises, and prints their counts on their own line (#11826).
- `parity` folds an idle alert into the same episode's previous one when it follows within two polls (a transient screen), and prints the folded counts overall and per kind; it pairs `session_exited` without its per-poller first-seen time, and pairs a `token` event the Python watcher labelled `<project>:<sid>` with the same event under its session name (#11826).

## [0.4.2] — 2026-11-24

### Fixed

- The Stop-hook dashboard footer link texts now read `[local]` and `[tail]` instead of `[localhost]` and `[tailnet]`, matching the Architect script; URLs are unchanged.
- The poller raises `plan-limit` only for the harness's own usage-limit banner at the start of a line, not for chat text such as "GitHub rate limit resets at 06:38Z".
- One idle episode raises one `idle` alert: a single not-idle read between two idle reads at most 150 s apart keeps the episode's epoch.
- An idle epoch that has already alerted does not alert again on a changed screen until that screen has itself been idle for 10 minutes.
- Token-watch alerts keep a session's resolved tmux name when the name lookup fails for one cycle, so the `ctx_main` label and the Architect's bucket swap stay correct.

## [0.4.1] — 2026-11-23

### Breaking

- Public structs `Config` (fields `dry_run_state`, `read_inbox`, `tm_health_socket`, `triage_rules`) and `PaneInfo` (`screen`) gained fields, and `InputState` gained `Poll`; struct literals and exhaustive matches outside the crate need updating. Shipped as a patch under the internal numbering policy.

### Added

- `il` module and the `trusty-architect-il` binary: the Intent Ledger reads and writes of `datg.py` and `ilio.py`, in Rust, on the same files and formats (`.trusty-mpm/datg/[<session>/]state.json`, `log.jsonl`, `archive.jsonl`, `.lock`). `trusty-architect-il datg <verb>` covers `init`, `add`, `update`, `close`, `act`, `acts`, `batch`, `session set`, `remind`, `remind-done`, `remind-fire`, `reminders`, `rebalance`, `prune`, `rotate`, `archive-show`, `show`, `digest` and `fresh`; `trusty-architect-il ilio <verb>` covers `table`, `parse`, `check`, `expand`, `relay`, `apply` (the ledger part: the dashboard store, steering log and `last-apply.json` stay in Python) and `collapse`. `hook`, `check-dispatch`, `stamp` and `ilio record` are not ported, and no caller is switched.
- A write takes the same `.lock` the Python scripts take, so mixed Python and Rust writers serialise, and gives up after 10 s with exit 2 and nothing changed. State is written atomically (temp file, fsync, rename, directory fsync), validated against the ledger schema before every write, and each log append is one `write`. A corrupt `state.json`, a failed step or a held lock exits non-zero; a state that advanced while its log line failed says so instead of passing as success.
- `state.json` keys keep their insertion order (an order-preserving JSON value), so a Rust write is byte-identical to the Python's on the live Architect ledger. The digest, `show`, `acts`, `fresh` and every write verb match the Python byte for byte in the golden tests; `tests/il_parity.rs` (ignored, needs python3) repeats the comparison on copies of the Architect and tm-dogfood ledgers. `TRUSTY_IL_NOW` freezes the clock for those tests.
- On copies of the live ledgers a release build runs a digest in about 5 ms (Python about 65 ms) and an apply in about 22 ms (Python about 99 ms).
- The Rust poller now raises the `token` alert kind: a port of the deployed `token_watch.step` (context buckets, cache-hit ratio, idle and unexpected cache misses, polling, polling rate, Opus share, burn, session churn, fan-out, watcher degraded), with its fingerprints, 24 h fired dedupe, rule cooldowns and the `token` 3600 s re-emit. Live runs share `inbox/token-watch-state.json` and write `local/usage/token-hourly.jsonl`; a shadow run keeps its own seeded copy and writes no rollup (MB1595 fix round).
- `sched`, the Architect's one job scheduler (MB1595 spike), and the `trusty-architect-sched` binary with the verbs `validate`, `status`, `run --once`, `daemon`, `cutover`, `rollback`, `parity` and `plist`. It loads `local/meta/registry.json` (the 47-job contract loads unchanged) and runs script and `claude -p --agent` jobs on `every` or UTC cron schedules, with jitter, catch-up, one-run-at-a-time overlap, a timeout that kills the process group, a daily cap, failure backoff and freshness escalation. Optional extension fields: job `timeout_s`, `overlap`, `cwd`, `env` and `schedule.tz` (only `UTC`), plus a top-level `poll` object.
- State is one file (`local/meta/sched-state.json`) and the run log is `local/meta/runs.jsonl`. The daemon takes a single-instance lock, rewrites `local/meta/sched.alive` each tick and keeps the previous config when `registry.json` fails validation; an empty or unreadable config exits non-zero.
- Shadow mode runs the poll cycle as a dry-run job against a scratch copy of the poll state and appends `inbox/shadow-wakes.jsonl`; `parity` joins it to `inbox/alerts.jsonl` and `inbox/poll.log` as matched / scheduler-only / poller-only counts. `cutover` and `rollback` move the wake duty between `poll-supervise.sh` and the scheduler and are idempotent; `plist` renders the `com.trusty.architect-sched` launchd user agent and loads nothing.
- The scheduler poller reads tm health over its socket each cycle and raises the same urgent `dep_degraded` alert as `fleet-poll.py`: 2 consecutive degraded reads of trusty-memory or trusty-search, `pending` ignored, one alert per dependency per outage. `TM_HEALTH=off` turns the step off; `TM_HEALTH_SOCKET` names the socket. An unreachable tm health socket raises `dep_health_error` (class `unreachable`) like an unparseable answer; streaks are held, never reset.
- `verbs::configured_interval` reads the poll cadence from `poll_interval_s` in `local/architect.json` (or the file `ARCHITECT_CONFIG` names), defaulting to 60 s, the same lookup as `architect_config.py`. A bad value, malformed file or missing named file is a usage error, never the default.
- `box_holds_text` checks that a captured input box holds typed text or a paste placeholder; `send_line_guarded` and trusty-mpm `session_send` share it, and `send_line_guarded` now accepts a paste placeholder as the echo (#10747).

### Fixed

- `tm architect poll --dry-run` now prints structured `would alert key:` and `would wake key:` JSON lines (session, kind, fingerprint, and whether a wake took the Haiku path) after a `shadow keys: v1` marker, and the shadow record reads them instead of the display text. Older shadow output still parses through the display path, which now keys the selftest, owner, dashboard and token displays correctly (MB1595 fix round).
- The scheduler's default shadow argv is now `tm architect poll --dry-run --shadow`, so the shadow cycle runs the ledger shim like the live poller, and a shadow dry run passes its own `ARCHITECT_DRY_RUN_STATE` file to the shim as `--state` instead of the once-seeded copy (MB1595 fix round).
- A shadow scheduler that starts (install or restart) reseeds its dry-run state from the live `poll-state.json` on its first poll run, so cycle 1 dedupes against the live poller's keys and replays no old alert; a reseed that cannot copy fails the run and is retried, never run on the stale state (MB1595 parity window 3).
- A shadow cycle takes each pane's `idle_since` from the live `poll-state.json` (read only), so the `idle` fingerprint carries the live poller's epoch; an unreadable live state logs `live idle ERROR` and keeps the shadow's own epoch.
- A shadow cycle raises the `reminder` alert for a reminder the live poller fired since its previous look (same fingerprint and detail as `fleet-poll.py`), which the dry ledger shim could not see; an unreadable ledger logs `live reminders ERROR` and rereads the same window next cycle.
- `trusty-architect-sched parity` leaves wakes no triage rule decided (the prototype's Haiku path) out of the verdict and both match percentages, reading the live side from `triage.jsonl`, and prints `haiku-path wakes excluded: N`.
- The shadow reads `events.jsonl` from the live inbox too, and its first plan seeds `pm_offset` and `events_offset` at the live files' current end, so the first read is no catch-up burst; every offset stays in the scratch state.
- A PM-message queue that exists but cannot be read keeps its offset instead of resetting it to 0 and replaying the queue later.
- The first shadow plan refuses to seed its live-queue offsets when a live queue cannot be stat'ed as a file or the dry-run state cannot be read or parsed: it returns an error and writes no marker and no state. Only a missing file reads as empty (D64).
- The Rust poller's `idle` alert now matches the deployed `fleet-poll.py`: it fires once per idle screen (the fingerprint carries a hash of the rows above the box, timers and clocks blanked) and no longer re-emits every 1800 s.
- The Rust poller applies `alert_triage.py`'s rules instead of waking for every alert, so a `ctx` alert below the 35% bar is logged, not woken; alerts the prototype sends to Haiku still wake, as in a Python dry run.
- The poll cycle raises `pm_message` from `pm-messages.jsonl` (a port of `pm_intake.py`), and a shadow reads the selftest, ctx-cycle and PM-message queues from the live inbox (`ARCHITECT_READ_INBOX_DIR`), so `selftest_fail` and `pm_message` reach the shadow.
- The input box reads `poll` when it holds only a stale poller wake line, as the deployed `input-state.py` does.
- `trusty-architect-sched parity` prints the alert and wake match percentages and a PASS or FAIL verdict (exit 1 on FAIL) against `--min-cycles` (default 120) and `--min-match-pct` (default 95); `cutover` uses the same verdict. A wake now counts only when the cycle's triage woke, not when a dry run repeats its unsent backlog.
- The dashboard pending port now counts an owner_answer item as relayed when `relayed` is `true`, a non-blank string, or `relayed_at` is a non-blank string, matching the deployed `dashboard_derive.py`.
- `trusty-architect-sched parity` pairs `idle` alerts and wakes on session, kind and screen hash, when their `since-<epoch>` parts differ by at most 300 s, one to one; the report prints how many idle pairs matched this way.
- `trusty-architect-sched parity` leaves `ctxwatch` alerts and wakes out of the match and counts them on their own line, since both pollers share one debounce state file.
- The scheduler's shadow poll now dedupes across cycles: `tm architect poll --dry-run` reads and saves its own state file when `ARCHITECT_DRY_RUN_STATE` names one, and the shadow plan seeds `local/meta/shadow/dry-run-state.json` once instead of copying the live state every run. Before, every shadow cycle re-alerted everything (new=347).
- The poll cycle (`tm architect poll`) raises one `owner_answer` alert per answered, cancelled or Architect-Do dashboard decision, fingerprinted `<id>@<at>` as `fleet-poll.py` does, re-emits it every 30 minutes while it stays pending, and raises `owner_answer_error` when the owner-queue store cannot be read. Before, an answer raised no alert.
- `tm architect poll --dry-run` with `ARCHITECT_DRY_RUN_STATE` no longer saves over a state file it could not read, and keeps a `.corrupt.<ts>` copy of a corrupt one before replacing it, as the live poll does.
- The `token` watcher now holds an exclusive `flock` on `token-watch-state.json.lock` from load to save, and saves through a per-process temp file, so a concurrent Python `token_watch.py` writer no longer loses its offsets or fired keys. A lock it cannot take within 30 s fails the scan instead of writing unlocked (#11629).

### Changed

- `sched/plist.rs` points at the canonical `trusty_common::launchd_labels::ARCHITECT_SCHED` label it mirrors.
- The scheduler's parity report pairs shadow and live events by identity first (session, kind and fingerprint; a live wake's fingerprint comes from its `triage.jsonl` id) within 15 cycles, then by session and kind within two cycles of the record, only when at least one side has no identity key (two keyed events with different identities never pair). It prints identity-only and identity-plus-fallback counts for alerts and wakes, the pairs each method made, and states that the verdict uses identity plus fallback; the 95% and 120-cycle thresholds are unchanged. Wakes compare per woken alert for records that carry wake keys (MB1595 fix round).
- `il_cron` alerts and wakes are left out of the parity comparison and printed as `il_cron excluded: N (unmeasurable in dry run: il-cron.py:249)`.
- The ledger session record limit (`SESSION_MAX`) is 8192 B, up from 2048 B, matching `datg.py` and `tm fleet init`. A record over it is still refused.

## [0.4.0] — 2026-11-20

### Added

- `poller::routines`: a `goals_dir` routine (goal_reconcile) raises `routine_stale` per active goal doc whose `reconciled_at` is missing, unreadable or older than the routine's stale threshold; an unreadable goal doc or folder is flagged too. A routine can set `stale_after_s` to override the default of twice `interval_s`; unknown keys are refused (#9779).
- Poller sensors from the live `fleet-poll.py`: queued `ctx-cycle-alerts.jsonl` failures (`ctxcycle`), `selftest-state.json` failures (`selftest_fail`), the hourly `data.kalloc.1024` zone watch (`host_zone`), and the task-complete refresh (`ctx` fingerprint `complete`) for PMs and the Architect (#9779).
- The wake line carries a one-line digest of the woken alerts and held PM messages (capped at 400 characters), the woken alerts are kept in `poll-state.json` until a wake is sent, a fresh `inbox/wake-suppress` marker holds the wake, and failed wakes are recorded in `pm-wake-alert.json` (#9779).
- The poller starts `self-clear.sh` at 50% of its own context when idle, and runs the prototype's Python steps on its cadence: `board_feed.py`, `share_sync.py poll`, `questions_rc.py` each cycle; `task_ingest.py --sweep`, `task_progress.py`, `il_dashboard_sync.py` every 60 s; `track_gh.py` and `runtime_selftest.py` every 5 min. A dry or shadow run starts none of them (#9779).
- `serve` and `verbs::serve`: the poll cycle as an event-driven service. An inbox write, a hot transcript append (mtime polling once a second) or a change in tmux pane activity (one `tmux list-panes -a` read per second; no tmux client is attached, in any mode including `--shadow`) starts the same `Runner` cycle after a 2 s debounce, at most once per 15 s (`SERVE_MIN_EVENT_GAP`); a 60 s tick still runs a cycle when nothing changed. The ledger shim runs at most once per 60 s in `poll` and `serve`. The service holds `poll.lock` (none with `--shadow`). A watcher that fails, a failed tmux read, or a panicking cycle is logged and raised as a `serve` alert while the tick keeps cycling; SIGTERM/SIGINT exit after the running cycle (#9779).
- `poller::slack` and `poller::push`: a failure the Architect has not cleared in 30 min is sent to the owner as a Slack DM from the MPM app bot when `ARCHITECT_PUSH_CHANNEL=slack` and `ARCHITECT_PUSH_SLACK_USER` names the recipient. The token is read from `SLACK_MPM_BOT_OAUTH_TOKEN` at send time and handed to curl on stdin; it never appears in argv, logs or alerts. One push per incident; a failed send (missing token, timeout, HTTP error, Slack `ok: false`, unknown channel, no recipient) is a `push_failed` alert and is retried no sooner than 30 min. Shadow and dry runs log "would push" and send nothing. Off by default (#9779, D5t).
- `poller::watchdog`: the Architect watchdog ported from `architect-watchdog.py` (dead, frozen, silent, ok; one relaunch per 30 min through `session-relaunch.sh --rc`, given up after two failures). It runs every cycle of `tm architect poll` (#9779).
- `poller::routines`: the D5r check raises `routine_stale` when a routine listed in `local/routines.json` has not run within twice its interval, and logs a failure left uncleared for 30 min as due a push. Nothing is sent; `ARCHITECT_PUSH_CHANNEL` ships unset (#9779).
- Shadow run (`Config::shadow`, `PollArgs::shadow`): writes only `inbox/shadow/` (alerts, state seeded from the live state, `shadow.log`), takes no lock, sends no keys, notifies nobody and relaunches nothing (#9779).
- The poller runs `scripts/fleet-poll-ledger.py` once per cycle (live and shadow, never in a dry run) for the ten ledger-bound steps that stay Python until the ledger port; a missing script, a child that cannot start, a timeout (killed after 240 s), a non-zero exit or unreadable output raises a `ledger_shim` alert in the cycle's failing set.

### Fixed

- `tm architect poll`: a malformed or invalid `local/routines.json` now raises `routine_stale` with fingerprint `config` instead of only logging, so the D5r check cannot switch off silently; a reset non-object `push` state record is logged (#9779).
- `poller::self_refresh`: a missing window or percent setting, a zero window, unreadable self-ctx tokens, a failed capture or a failed `self-clear.sh` start is now a `self_refresh` alert (one fingerprint per kind) in the cycle's failing set, due a push like any failure; the Architect pane's own capture and self-ctx read failures raise it too (#11541 review, #9779).
- `tm architect serve`/`poll` find panes when the process has no UTF-8 locale: tmux runs with `-u`, so it no longer rewrites the TAB in `list-panes -F` output to `_`. A `list-panes` row the parser cannot read is kept in `Listing::unreadable` and logged at warn level instead of dropped silently (#11543).

### Changed

- Context bars follow `ctx_bar.py`: PM `ctx` alerts at 31/35% (were 22/25%); the Architect's `self_ctx` threshold is its bar from `thresholds.architect_bar_ctx` in `il-schedule.json`, 35% by default (#9779).
- The builder band stops on swap activity of 256 pages or more per sample, not on swap left in use; the band detail reads `swap activity N pages` (#9779).
- No 5-hour `plan-limit` alert; `tm-architect-questions` raises no `idle` alert; the ILIO trial sessions are not watched; `parse_pane` reports `rc_active` (#9779).

## [0.3.0] — 2026-11-20

### Added

- `ledger::il`: the IL packet parser and checker ported from `ilio.py` (last ```` ```il ```` fence, required fields, field order, 240-character lines, `pct`, ledger ids, BADID look-alike ids, the `remind:` form, a `pct` unchanged while `done:` claims progress, `new:` kind, `act:` parts and datg's act limits), and `il::apply`, which writes a checked packet to a session's `datg` store with the bytes `datg.py` writes. A replayed packet is a no-op (G0b).
- `il::route` sends a block the Rust writer does not fully cover to `ilio.py apply`, and `il::mark_applied` records an ilio-applied packet so its replay is a no-op (G0b).
- `StorePaths::at`, `Writer::batch_at` and `writer::seen`, so a batch can target the `.trusty-mpm/datg/<session>` store (G0b).
- The IL checker refuses `ilio.py`'s own `SECRET` shapes (`password|secret|token|api_key` assignments, `sk-` keys, private-key headers, any case) as well as the injected scan (G0b).
- An `il::apply` whose `act` lines fail after the batch commits is no longer a replay: applying the packet again appends the acts once, and the packet replays only after they land (G0b).
- `il::hold`, `il::mark_pending`, `il::clear_pending` and `il::pending_unknown`: a per-store apply lock and pending markers, so one packet runs `ilio.py apply` at most once (G0b).
- `il::apply` and `il::route` write only what the caller hands them; the `tm hook` caller hands them the session's own turn only, never a handback or inbound message (see G0b IL authority (owner decision pending)).

## [0.2.3] — 2026-11-16

### Added

- `send_line_guarded` types one line through `send_guarded` and presses Enter only after the line echoes in the box; otherwise it returns the new `SendError::NotEchoed` (#11365).
- `ledger::fresh` ports the `datg.py fresh` and `datg.py ready` verdicts (exit 0, 1 or 2) so `tm architect restart` can gate on a fresh Intent Ledger checkpoint; an unreadable ledger is never fresh (#11366).

### Fixed

- The dashboard store stamps `created` and `updated` from its own clock on every write and ignores a client value for either; `created` keeps the stored value, and one batch uses one write time for every document and its change-log line (#10678).
- Dashboard store reads (`get`, `list`, `snapshot`) take `<root>/.lock` shared with the store's bounded wait, so a read never lands between the files of one batch and the task, session and header documents of one write are seen together; `snapshot` reads every collection under one lock. A read behind a writer that never lets go is `Unavailable` within the wait (#10679).
- A pending "answered" event now carries the owner's typed `answer_note` after the button label (`Other: <text>`), matching the Python prototype (#10765).

## [0.2.2] — 2026-11-13

### Fixed

- A dashboard schema `pattern` with an escape class the loader refuses now names that escape (for example `\s`) instead of always saying "like \d" (#10697).
- The dashboard store refuses a `set`, `update` or batch that writes `meta/settings` `technical_level` or an item's `level_override` outside the six technical levels, whatever the schema mode. A refused op aborts the batch and leaves the store unchanged (#11064).

### Changed

- Reformatted the `request!`/`row!` macro calls to the rustfmt 1.100 layout; no behavior change.

## [0.2.1] — 2026-11-09

### Added

- The ledger writer reads and keeps an answer's `source`, and writes the Architect form (`update` op `source`: `{by: architect, basis}`) in byte parity with the Architect's `datg.py`. It refuses `{by: owner, …}`: only `datg.py`, which checks the quote against the owner-turn record, writes an owner source (#10780).

### Changed

- The dashboard schema check knows the `watch_changes` collection, so a store write of the owner's Watch or Unwatch request is validated against `watch_changes.schema.json` instead of refused as a collection with no schema (#10896).

## [0.2.0] — 2026-10-30

### Added

- `dashboard::rpc`: the twelve `mpm.architect.dashboard.*` methods (`get`, `list`, `set`, `update`, `delete`, `batch`, `pruned`, `validate`, `fsck`, `tally`, `footer`, `ids`), one request type per verb that refuses unknown keys, and `DashError::code`: a stale `if_version` exits 1, a refused request 2, an unreadable or locked store 3 ([#10063](https://github.com/bobmatnyc/trusty-tools/issues/10063))
- `dashboard::verbs`: one body per verb over the store a request names (an absolute root; for a write, the prune log outside it), a `set` with no `if_version` on an existing document refused as a conflict under the store lock (`Store::with_blind_set_refused`), so of two concurrent blind `set`s of one new id exactly one wins, a prune log not named `pruned.jsonl` refused for both `delete` and `pruned` (exit 2), and `render`, which prints each reply as `dashboard-db.py` does ([#10063](https://github.com/bobmatnyc/trusty-tools/issues/10063))
- `fsck` reports a live record whose `goal`, `depends_on` or `links` entry names an id no document holds (LDG-2.2.5), and reports `references not checked: <error>` when the id collections cannot be read ([#10063](https://github.com/bobmatnyc/trusty-tools/issues/10063))
- `dashboard::pending` ports the prototype's owner-event rules (`pending`, `stale_tasks`, `last_activity`) and `dashboard::derive` gains `run_progress`, `ordered` and `goal_sessions`, all pure functions of a snapshot and `now` whose output is byte-equal to `dashboard_derive.py`; unparseable timestamps keep the prototype's result and are also listed by `skipped()` (#10403).

### Fixed

- The first logged batch on a dashboard store creates `changes.jsonl` before its commit, with one `baseline` line per document already on disk: `fsck` no longer reports a legacy document as having no change line, and still finds a first batch whose change-log append failed ([#10063](https://github.com/bobmatnyc/trusty-tools/issues/10063))
- Dashboard store writes (documents, the change log, the prune log) open every directory below the store root, or below `/` for the prune log, without following a symlink, so a directory swapped for a symlink after a check can no longer redirect a write ([#10063](https://github.com/bobmatnyc/trusty-tools/issues/10063))
- `dashboard` batch ops refuse a key outside `op`, `collection`, `id`, `data`, `if_version` (exit 2), so a misspelled `if_version` fails the whole batch instead of dropping the stale-write guard ([#10063](https://github.com/bobmatnyc/trusty-tools/issues/10063))

## [0.1.0] — 2026-10-26

### Breaking

- `Waker` and `Runner` gain the required field `messages: &dyn MessageSink`, and `verbs::poll` takes the sink as a third argument; `Waker` no longer derives `Debug`. Pass `NoMessages` to record nothing ([#9167](https://github.com/bobmatnyc/trusty-tools/issues/9167))
- `Runner` gains the required fields `usage_probe: &dyn ProbeLauncher` and `notifier: &dyn Notifier`, and `Sensors` gains `usage_probe`; `verbs::poll` passes `ScriptProbe` and `Osascript`, and tests pass `RecordingProbe` and `RecordingNotifier` ([#9801](https://github.com/bobmatnyc/trusty-tools/issues/9801))

### Added

- The poller reports each wake it submits to the caller's `MessageSink`: one `PaneMessage` (kind `PollWake`, the session, the pane, the exact text typed), recorded after the Enter. A skipped, dry-run, unconfirmed or erased pointer is not recorded. A sink error never stops the wake; it is appended to the logged phrase as `(message event not recorded: <reason>)` ([#9167](https://github.com/bobmatnyc/trusty-tools/issues/9167))
- Rust ports of the Architect prototype's pure logic, held to byte-identical JSON against the live prototype scripts: pane parsing, alert evaluation and dedup (`classify`, from the classifier inline in `fleet-poll.py`, the poller the live host runs — 22/25 ctx bar, timed agent rows anywhere on screen, `current_waiting`, and the `turn_active`, `account` and `rc_failed` fields), the input-box classifier (`classify_input`, from `input-state.py`), the quiet-session list (`quiet`), and the Architect's own context meter (`self_ctx`) ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `input_state_for_send`, the fail-closed input-box reading for `send_guarded`: a capture it cannot positively read as an empty or suggestion box never reads as accepting input, and a suggestion is accepted only when every visible character in the box is dim ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `to_python_json`, which writes JSON in Python's `json.dumps` form ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `poller`: the sensors and state half of the `fleet-poll.py` port, held to byte-identical goldens from the frozen prototype — `Config` (environment read once; a bad `PANE_CTX_WINDOW`, `SELF_CTX_WINDOW` or `SELF_CTX_THRESHOLD` switches off only the sensor that reads it and is listed in `Config::problems`, as the prototype logs and carries on), `list_panes`/`capture`, `read_events`, `host_alerts` and the builder band behind a `HostProbe` seam, `TranscriptCtx` (a pane's ctx from its own transcript via the live session registry, cached by mtime and size), `self_ctx_alerts`, and the `alerts.jsonl` / `poll.log` appends ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `PollState` with `save_state`, which writes `poll-state.json` atomically (a uniquely named `create_new` temporary file, fsync, rename, directory fsync; a failed save removes its temporary file, so concurrent writers never share one) and keeps every key it does not know, and `load_state`, which reports a corrupt file instead of hiding it ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `Keys` / `NamedKey`: `send-keys` in literal mode (`-l --`, so a dash-leading text reaches the pane as text) or named-key mode (`Enter`, `BSpace`) ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `poller::Waker::wake` and `poller::dispatch_wake`, the Rust port of the poller's wake: the pointer is typed into the Architect's input box only when the pane's live session is `tm-architect` and the box reads `empty` or `suggestion`; Enter goes out only when the box text equals the pointer, and cleanup erases only the poller's own trailing text, never into an empty box; the owner is re-read before every send, and the pending count clears only on a result starting `sent` ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `Clock` (`SystemClock`, `FakeClock`) and `ScriptedTmux`, a tmux fake that answers each verb from a script and records every argv ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `poller::Runner`, the Rust port of the poller's cycle and loop: `run_once` lists the panes, renames `tm-*` windows with exact `=S:IDX` targets, raises `exited` for shell panes and vanished sessions (never for test-fixture sessions), reads ctx from a pane's own transcript when the status line has none, drops `idle` alerts of quiet sessions, dedups, appends to `alerts.jsonl`, triages and wakes; nothing from a cycle is committed before its alerts are written, so a failed sensor, a failed append or a kill re-emits the same alerts ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `Runner::run_loop` refuses a zero, negative or non-numeric interval before any tmux call, holds `<inbox>/poll.lock` (`PollLock`) so one poller runs per inbox, and sleeps the interval exactly once after every cycle, a failed one included; a dry run takes no lock, writes no file and sends no keys ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `PollSession` never saves over a `poll-state.json` it cannot read, keeps a `poll-state.json.corrupt.<unix>` copy before resetting a corrupt one (a failed copy leaves no file behind), and keeps wakes owed while the file was unreadable ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `PollState` types `wake_pending`, `wake_logged` and `wake_sent_at`; a stored value that is not a count loads as `StoredCount::Unreadable` and still counts as a wake owed ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `architect_busy`, `rename_windows` / `plan_window_renames` and the `Triage` seam with its `WakeAll` default ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `read_events` holds its offset at the last newline, so an event still being written is read whole next cycle; a bad `LOAD_FACTOR` is listed in `Config::problems` and turns off host alerts only, where it used to fail `Config::from_env` ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `Runner::run_once` logs each bad `Config::problems` entry, with the sensor it turned off, on every cycle
- A `poll-state.json` that is not valid UTF-8 is treated as corrupt (copied to `poll-state.json.corrupt.<unix>`, then reset and saved over) instead of an I/O error that blocked every save; a real OS read error still blocks saves ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- New leaf crate `trusty-architect` for the trusty-mpm Architect: a `Tmux` trait with a real `SystemTmux` and a recording `FakeTmux`, exact `%N` pane ids, and `send_guarded`, which refuses with an error instead of sending keys into an input box that holds typed text ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `ledger`, the PM Intent Ledger contract (phase P0 of the IL Rust port, not yet used by `tm`): typed `state.json`, `archive.jsonl` and `log.jsonl` records that write back the prototype's bytes and keep unknown keys, one id and code grammar, the title, paragraph and size-bar limits, minute-to-second timestamp normalization, a `schema` version that a writer refuses when newer, the `.trusty-mpm/dbatg/<session>/` layout anchored to the launch directory, a per-host code registry behind `init_store`, and the `mpm.dbatg.*` request and response types, which refuse unknown keys ([#9757](https://github.com/bobmatnyc/trusty-tools/issues/9757), [#10062](https://github.com/bobmatnyc/trusty-tools/issues/10062))
- `ledger::writer`, the PM Intent Ledger writer: a batch applies every op or none under the store's `flock` (bounded wait, exit 3 past it), stages a commit journal so a writer killed after the log append is completed by the next one, answers a retried `request_id` from its `log.jsonl` record instead of applying it twice, refuses a secret-shaped value and logs the refusal without it, logs `tamper` and does not overwrite when `v` moves mid-write, prunes once before refusing a write over the 16 KB bar, and leaves the prototype's `state.json`, `log.jsonl` and `archive.jsonl` bytes; `Op::check` now enforces option count and length, `finish` and every session-record limit, and `StoreRef::paths` normalises the project path ([#9758](https://github.com/bobmatnyc/trusty-tools/issues/9758))
- The ledger writer fails closed: a read error other than a missing file refuses the batch (non-UTF-8 log and archive bytes decode lossily, a non-UTF-8 journal is left for repair), a log line the prototype appends without the lock no longer wedges the store, secret scanning covers each decoded value and the `request_id`, orphan archive lines above `v` are dropped before their `cv` is reused, and the store directory is fsynced after the `state.json` replace and before the journal is removed ([#9758](https://github.com/bobmatnyc/trusty-tools/issues/9758))
- The PM ledger writer appends one `outbox.jsonl` line per changed record, `{seq, epoch, code, id, rev, op, at}`, in the same journalled commit and before the `state.json` replace: `seq` is gapless per store, `rev` is the commit's `v`, a record that left `state.json` is named as `archived`, and a refused, no-op or replayed batch appends none (LDG-4.4, [#9758](https://github.com/bobmatnyc/trusty-tools/issues/9758))
- `poller::steps::usage`: each cycle samples Claude account usage into `local/usage/ledger.jsonl`, starts a `/usage` probe (`scripts/usage-probe.sh`) in one idle pane when one is due, and raises the prototype's `usage` alerts (5h exhaustion within 45 min, weekly 80/90%, cutover recommended), byte for byte against `usage.py`. A probe counts only after it starts, so a failed launch is retried and its cycle keeps its alerts; a sampler failure is one `poll.log` line and leaves the stored usage state as it was. `burn_rate` returns no rate where the prototype raises `IndexError` (any account's newest reading over 48 h old) ([#9801](https://github.com/bobmatnyc/trusty-tools/issues/9801))
- `poller::steps::notify_macos`: each new `architect_*` alert is a macOS notification through `osascript`, bounded at 10 s; a failure, including a non-zero exit, is a `notify ERROR` log line and changes no state ([#9801](https://github.com/bobmatnyc/trusty-tools/issues/9801))
- `Config::usage` (`UsagePaths`), read from `USAGE_LEDGER`, `USAGE_STATUSLINE_CACHE`, `USAGE_CLAUDE_JSON` and `CLAUDE_CONFIG_DIR` as `usage.py` reads them ([#9801](https://github.com/bobmatnyc/trusty-tools/issues/9801))
- The `/usage` probe is killed with its process group if it is still running after 120 s, and a probe that started stays recorded when the cycle then fails, so a failing cycle does not launch it again each minute ([#9801](https://github.com/bobmatnyc/trusty-tools/issues/9801))
- `verbs`, the library behind `tm architect poll|classify|input-state|quiet|self-ctx`: each verb prints what the Architect's Python script printed for the same input, and fails with exit status 1 (bad input, tmux, or state) or 2 (bad argument) where the script printed a default and exited 0 ([#9822](https://github.com/bobmatnyc/trusty-tools/issues/9822))
- `Runner::run_single`, the poller's `--once`: one cycle and no sleep, returning `RunError::Incomplete` when tmux cannot list panes or the state file cannot be read and saved, and `RunError::BadConfig` (naming each variable) when `LOAD_FACTOR`, `PANE_CTX_WINDOW`, `SELF_CTX_WINDOW` or `SELF_CTX_THRESHOLD` is bad; the loop still runs with that sensor off ([#9822](https://github.com/bobmatnyc/trusty-tools/issues/9822))
- `Config::supervisor` and `Config::poll_session` (default `tm-architect` and `tm-architect-poll`), set by `ARCHITECT_SESSION` and `ARCHITECT_POLL_SESSION` through `verbs::poll_config`; the cycle skips both sessions and their events, and `Waker::dispatch_as` wakes only the configured supervisor ([#9822](https://github.com/bobmatnyc/trusty-tools/issues/9822))
- `quiet::read_quiet_sessions_checked`, which returns the read error instead of an empty list ([#9822](https://github.com/bobmatnyc/trusty-tools/issues/9822))
- A dry-run cycle logs the prototype's per-pane line (`  <session> <pane> cmd=… box=… alerts=…`) ([#9822](https://github.com/bobmatnyc/trusty-tools/issues/9822))
- The dashboard store appends one `changes.jsonl` line per changed document after each commit, in both formats: `{by, collection, id, keys, op, seq, sha256, ts, version}` with a strictly increasing `seq` and the SHA-256 of the bytes written; it rotates by month into `changes-YYYY-MM.jsonl` without ever overwriting a rotated file; a batch is refused, with nothing written, when the log's last line is torn or has no `seq`, its first line's `ts` is bad or the rotation target exists, and a batch whose durable append fails after the commit returns `Unlogged` (the documents stay committed) instead of succeeding silently ([#10063](https://github.com/bobmatnyc/trusty-tools/issues/10063))
- `PruneLog`, the dashboard's delete archive at a configurable path outside the served root, checked as the OS resolves it (symlinks before `..`; a `..` below a missing directory, a dangling symlink or an unresolvable path is refused) and written at that resolved path without following a symlink at its last component, so a link planted after the check fails the delete: each deleted document's full record `{by, collection, data, deleted_at, id, version}` is appended and fsynced before the delete, a failed append leaves the document in place, and `lines(id, collection)` returns the raw records, prototype lines included ([#10063](https://github.com/bobmatnyc/trusty-tools/issues/10063))
- `Store::fsck` (corrupt or non-canonical files, change-log gaps, reversals and torn lines, a document whose version or bytes differ from its last change line, a deleted document still present, a logged document missing, and, once a change log exists, a document with no change line) and `Store::validate` (every document against its schema plus the prototype's goal and item cross references); a missing store root is an error, never clean ([#10063](https://github.com/bobmatnyc/trusty-tools/issues/10063))
- The dashboard store refuses an op holding a secret-shaped value, naming the op and the shape but never the value, and refuses every write when no secret scan is configured; the scan walk is shared with the PM ledger writer ([#10063](https://github.com/bobmatnyc/trusty-tools/issues/10063))
- The dashboard store refuses a write of an id or collection that differs only in letter case from an existing one or from one earlier in the same batch, in both formats, and a read never answers one id with another id's file (on APFS `A01.json` and `a01.json` are one file); `media` ids share the per-prefix id space ([#10063](https://github.com/bobmatnyc/trusty-tools/issues/10063))
- `dashboard`, the Architect dashboard store core (not yet wired into `tm`): a `Store` over the prototype's `dir` and `jsonl` formats that leaves the prototype's bytes, refuses a store in the other format instead of reading it as empty, applies a batch all-or-nothing under `<root>/.lock` with a bounded wait, writes nothing on a stale `if_version`, refuses a body that carries `version`, restores earlier files when a later write fails (and returns `RollbackIncomplete`, naming the files, when a restore fails too), and refuses a delete unless a `DeleteArchive` records it first; the recycled id space (`next_id`, `free_count`, `retire_candidates`), `tally` and the reply `footer` as the one count path (an unreadable store is an error, never a zero), and a schema hook with a port of the prototype's keyword-subset validator, covering the `media` collection, whose `pattern` matcher runs in linear time with stack use independent of the input ([#10063](https://github.com/bobmatnyc/trusty-tools/issues/10063))

### Fixed

- `poller::dispatch_wake` reads a stored `wake_pending` that is not a non-negative integer (a string, a fraction, a negative) as 1 instead of 0, so the wake still runs and only a result starting `sent` clears the count ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))

### Changed

- `send_guarded` takes a `SendReading`, which only `input_state_for_send` produces, and a `Keys` value; `classify_input`'s reading no longer type-checks there ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `SystemTmux` bounds every call by a timeout (10 s by default): past it the child is killed and reaped and the call returns `TmuxError::Timeout`; output is drained on threads, so a large stdout cannot deadlock ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- Pane parsing reads every Unicode decimal digit as Python's `re` `\d` does, so `ctx` with Arabic-Indic digits parses as a number ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- The send guard's dim check fails closed on a non-SGR CSI and on C1 controls, which could reset attributes mid-row ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `Tmux::run` refuses the `send-keys` verb with `TmuxError::SendKeysRefused`; `Tmux` implementors now write `execute`, which only `send_guarded` and `run` can call (it takes a `Permit` that has no public constructor) ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `SendReading` is no longer `Copy` or `Clone`, and `state` / `accepts_input` borrow it, so one reading allows one send ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- Ledger ids are ephemeral: `State::allocate` and an explicit `add` id now refuse only a live id, so an archived id is issued again (the wrap order is unchanged, and a full space of live ids is still `NoFreeId`); `allocate` no longer takes an `archived` slice and the writer no longer reads `archive.jsonl` for ids ([#10235](https://github.com/bobmatnyc/trusty-tools/issues/10235))

