# SPEC-IL-01 — the Intent Ledger: durable session intent and continuous context

Status: DRAFT, 2026-10-17. Owner: the owner. Name: Intent Ledger (IL), D95.

Brief: `local/briefs/2026-10-17-il-continuous-context.md`. the owner's ruling (04:14Z): "Let formalize IL and resolve q10 issues. Spec and build any needed parts, include for all PMs, it should be sufficient for handoff/resume. Goal is to get to continuous context management, no explicit pause/resume step. Clear of all defined signals and staleness threshold."

Related: `local/briefs/2026-10-09-pm-datg-prototype-design.md` (DATG design), `2026-10-11-datg-dogfood-install.md`, `2026-10-12-t08-datg-pilot-score.md`, `2026-10-17-stale-context.md`, `2026-10-17-stale-context-baseline.md`, `docs/specs/DRAFT-workflow-artifacts.md` (section 7.3 marks the per-PM ledger as a proposal; this spec supersedes that proposal).

## 1. Purpose and scope

The IL is the single durable record of a session's intent and in-flight state. It is sufficient to resume the session after `/clear` with no pause snapshot. A resume reads the IL digest (section 6) and nothing else.

Applies to every PM session and to the Architect. A PM's IL lives in its project; the Architect's IL is the dashboard store plus its own session record (section 10).

Not in scope: long explanations (they live in briefs the IL links to), code, secret values (locations only), the transcript.

Today's pieces, unified here: `scripts/datg.py` (goals, decisions, asks, tasks), `scripts/datg-hook.sh` (hooks), `scripts/dashboard-db.py` and `scripts/il-archive.py` (Architect level), `/tm-session-pause` snapshots (retired for refreshes, section 11).

## 2. Data model

One directory per project session scope: `<project>/.trusty-mpm/datg/` (path kept; the name datg stays as the file prefix). Where several PMs share a project directory (trusty-tools), each PM has its own directory `datg/<session>/`; the hook filter already selects by tmux session name. Files:

| File | Role | Writer |
| --- | --- | --- |
| `state.json` | active items and the session record | the PM, atomically, under flock |
| `log.jsonl` | append-only change log | `datg.py` |
| `archive.jsonl` | closed items, one per line, queryable by id | `datg.py prune` / `close` |
| `log-<YYYYMMDD>.jsonl` | rotated logs | `datg.py rotate` |

### 2.1 Existing items (unchanged fields)

`state.json` keys: `code` (str, 2-8 chars), `v` (int, +1 per write), `config` ({mode, block_after}), `goals[]`, `items[]` (D and A), `tasks[]` (T), `next` ({G,D,A,T: hex}), and new: `session`.

- Goal: `id` (`<code>-G<hex2>`), `title` (<= 120 chars), `rank` (int), `status` (active|done|parked), `finish` (<= 160), `maps` (Architect goal id).
- Item (D/A): `id`, `kind`, `status` (open|answered|done), `goal`, `title` (<= 120), `opts[]` (<= 4, `l` <= 160 chars, `rec` bool), `answer` (<= 300), `outcome` (<= 300), `esc`.
- Task: `id`, `goal`, `title` (<= 120), `status` (open|running|done), `to`, `outcome` (<= 300).

The title limit drops from 200 to 120. Today's open task titles run to about 250 bytes and are the largest active field.

### 2.2 New: session record `state.json["session"]`

One object, rewritten whole. Target size 1 KB, hard limit 2 KB.

| Field | Type | Limit | Meaning |
| --- | --- | --- | --- |
| `task` | str | 160 | current task: one line, with its id (`df-T04`) |
| `next` | str | 200 | the next step, imperative, enough to act on cold |
| `waiting_on` | list of {who, what, since} | 5 entries; what <= 120 | `who` is owner, architect, ci, agent, other |
| `owed` | list of {check, expect, ref} | 8 entries | owed checks with exact values: `check` (<= 80, e.g. "main sha after merge"), `expect` (<= 80: SHA, run id, expected main, count), `ref` (<= 80: PR or issue url) |
| `agents` | list of {id, goal, what} | 6 entries; what <= 80 | running agents (harness agent ids) |
| `crons` | list of {id, schedule, prompt} | 4 entries; prompt <= 200 | session crons and Monitors, enough to re-create them |
| `boundary_at` | ISO minute UTC | | last task boundary time |
| `ctx_use` | {at, active, stale, ctx, keep} | `keep` <= 160 | last CTX-USE: `active` and `stale` ints 0-100, `ctx` int tokens, `at` ISO minute |
| `refs` | list of {key, value} | 12 entries; key `^[a-z0-9][a-z0-9_.-]{0,31}$`; value one line <= 200 | pointers the owner refers to (doc URL, source md, relay marker); printed first under POINTERS in the digest |
| `written_at` | ISO minute UTC | | time of the last write that changed state |

Rules: no field holds secrets; a value that would be a secret holds its location. `owed.expect` holds the exact value, never "latest" or "main". The Architect checks `owed` entries by command, not by the PM's word (ARCHITECT.md r-071).

Verb: `datg.py session set --task .. --next .. [--wait who:what ..] [--owe check=expect@ref ..] [--agent ..] [--cron ..] [--boundary] [--ctx-use A:S:CTX:keep] [--ref KEY=VALUE ..]`. `session set` can ride in the same `batch` write as item ops, so a turn end is still one write.

## 3. Size bar and archiving (Q10 gap 1)

### 3.1 What is in today's 58 KB

Measured on `trusty-tools/.trusty-mpm/datg/state.json`, 59,395 bytes, 2026-10-17 04:14Z:

| Part | Bytes | Share |
| --- | --- | --- |
| `items` (83 entries: 41 done, 38 answered, 4 open) | 56,018 | 94% |
| of which done | 28,994 | 49% |
| of which answered | 25,281 | 43% |
| of which open | 1,577 | 3% |
| `tasks` (19: 13 open, 6 done) | 4,963 | |
| `goals` (4) | 879 | |
| `config`, `next`, `code`, `v` | about 130 | |

`log.jsonl` is 167,094 bytes in 901 lines over 6 days (about 28 KB a day), never rotated.

### 3.2 What is removed

On the first prune, 79 of the 83 items leave `state.json` (all 41 done and all 38 answered): 54,275 bytes. The 6 done tasks leave (1,755 bytes). What stays: 4 open items (1,577), 13 open tasks (3,208), 4 goals (879), config and counters (about 130), plus the session record (about 1 KB). Projected size: 6.8 KB. The 2 KB bar of the DATG design cannot hold even this active set, because 13 open tasks alone are 3.2 KB.

Answered items are not kept active. An answered item whose answer is already relayed and acted on is `done` (its outcome recorded) and moves out. An answered item whose answer is not yet acted on stays, and is by definition recent.

### 3.3 The bar (measured, replaces 2 KB)

- Target: `state.json` <= 8 KB. Alarm: > 8 KB writes a `size_warning` line to `log.jsonl` and puts a line in the next digest. Hard refuse: > 16 KB (a write is refused, exit 2, with the largest items named).
- Why 8 KB: the measured active set is 6.8 KB with the 120-char title limit; 8 KB leaves 1.2 KB headroom, and 8 KB is about 2,000 tokens, which fits the 3k-token digest (section 6) with the log tail left out. A 2 KB bar needs under 5 open tasks and 2 open items; the pilot shows 13 and 4 under normal load.

### 3.4 Archive on close

- `datg.py close` (and `update --status done`) moves the item to `archive.jsonl` in the same atomic write. `archive.jsonl` line: the full item plus `closed_at`. The move is part of the commit, so a crash leaves the item in exactly one file.
- `datg.py archive-show <id>` prints one archived item (queryable by id). An index is not needed; the file is scanned, and a line is about 400 bytes.
- Answered, relayed, acted-on items are closed by the PM's own rule 6 ("close with a verified outcome"); `datg.py prune` closes any `answered` item older than 24 h whose `outcome` is set, and moves any `done` item immediately.
- `il-archive.py pm-ledger prune` keeps its role as the memory step: it copies `archive.jsonl` lines older than 7 days to trusty-memory, then appends them to `archive-<YYYYMM>.jsonl` beside it and removes them from `archive.jsonl`. Nothing is deleted (the owner D2b, 2026-10-17). Memory is written first (its existing rule), the keep-file append second (fsynced; on failure `archive.jsonl` is left untouched), the rewrite last.

### 3.5 Log rotation

`datg.py rotate` runs from the scheduler (section 4): when `log.jsonl` exceeds 128 KB or holds events older than 7 days, lines older than 7 days move to `log-<YYYYMMDD>.jsonl`; files older than 30 days are compressed to `log-<YYYYMMDD>.jsonl.gz` and kept; nothing is deleted (the owner D2b, 2026-10-17). `il-archive.py` may still copy them to memory as one note per file. Both log shapes (`t`/`ev` and `ts`/`kind`) are kept as written; the reader accepts both. The log is audit data and is never read at resume.

## 4. Scheduled work (Q10 gap 2)

No cadence job depends on Architect memory. One deterministic scheduler runs them: `scripts/il-cron.py`, called by the existing poller `scripts/fleet-poll.py` once per hour (one more step in its cycle; no second monitor, per the hard limit). State: `inbox/il-cron-state.json` (job name, last run, last result). A job that is overdue runs at the next cycle; a job that fails is retried each cycle up to 3 times and then raised as a poller alert.

| Job | Cadence | Action |
| --- | --- | --- |
| `prune` | hourly | `datg.py prune` on every installed project; `il-archive.py dashboard retire/prune` |
| `rotate` | hourly check | `datg.py rotate` |
| `size-check` | hourly | alert if any `state.json` > 8 KB |
| `pilot-rescore` | on a date in `local/il-schedule.json` | compute the pilot measures of the T08 score (escalations through pm-notify, drift warns that changed a dispatch, block-mode false blocks, state size) and append the result to `local/briefs/`; alert the Architect with the file path |
| `stale-check` | each full-poll pass | (section 7) |

Any other cadence job (an extended-cycle rescore, a weekly review) is one entry in `local/il-schedule.json`: `{name, due, every, command}`. The missed 10-15 rescore is entry `pilot-rescore` with `due: 2026-10-17` on first install. The Architect adds entries; it does not run them.

## 5. Write points

Hook set per PM (all through `scripts/datg-hook.sh <session>`):

| Event | Action |
| --- | --- |
| SessionStart | inject rules and the digest (section 6); after a `/clear` or a restart this is the resume |
| UserPromptSubmit | none (no write); injects nothing |
| Stop (end of every assistant turn) | run `datg.py stamp`: if the turn changed state files, nothing; otherwise compare `written_at` with the turn end and append a `stale_il` line to `log.jsonl` when state changed but the PM did not write |
| PreToolUse(Agent\|Task) | dispatch check (existing) |

Who writes: the PM, one `datg.py batch` call at the end of every turn (existing rule), now including `session set`. At every task boundary the same batch also carries the CTX-USE line (`--ctx-use`). Task boundary: the PM's "done:" message, issue closed, PR merged, or "Nothing is running".

Freshness rule: the IL is fresh when `written_at` is later than the end time of the last assistant turn that changed state (a tool call that mutated a repo, a dispatch, a merge, a relayed answer, a cron change). The Stop hook computes it: `datg.py fresh` exits 0 when fresh, 1 when stale, 2 when the file is unreadable. The poller and `ctx-cycle.py` call it. A PM that does not write within one turn is stale; two consecutive stale turns raise a poller alert.

The P10 conflict found by the pilot (PM rule against non-git Bash vs per-turn `datg.py` calls; ruling D01: no carve-out, writes batched at handback boundaries by an agent) stays open for the per-turn write. This spec requires the per-turn write; the build list (item B2) asks for a `tm datg` verb in trusty-mpm so the write is a daemon call and not a Bash call. Until then, the batch write at each handback boundary is the minimum; the freshness rule applies at that granularity and a boundary write is mandatory before any clear.

## 6. Resume digest

`datg.py digest [--project DIR] [--max-tokens 3000]` prints a bounded digest to stdout. Budget: 3,000 tokens (about 12 KB); the active set at the 8 KB bar fits. Order (cut from the end first when over budget):

1. header: session, code, `written_at`, freshness, ctx before the clear if known
2. session record: task, next, waiting_on, owed (exact values), agents, crons, boundary_at, ctx_use
3. active goals in rank order (id, title, finish)
4. open and answered items (id, title, answer, recommended option)
5. open and running tasks (id, goal, title)
6. last 10 log lines of kind `escalated`, `answered`, `done`, `drift`
7. pointer lines: `archive.jsonl` path, brief paths named in item titles

The digest never includes archived items, rotated logs, or transcript text. Over budget: it drops section 6, then 5's oldest tasks, then prints `TRUNCATED: <n> bytes dropped` as the last line.

Resume line, sent by `ctx-cycle.py` after `/clear` (one line, under 600 characters, relay marker format):

`[<Name> Architect HH:MMZ] Context cleared by the IL cycle. Resume: run python3 <root>/scripts/datg.py --project <dir> digest, crons and Monitors survive the clear and keep firing: re-create none (CronList will not show them; the digest keeps their ids), verify each owed check by command, then continue "next". Do not run /tm-session-resume.`

The SessionStart hook also injects the digest, so a clear that the hook sees needs no second read; the resume line remains the explicit instruction and the fallback when the hook is filtered out.

## 7. Clear signals and thresholds

Measurements:

- `ctx`: input + cache_read + cache_creation of the last assistant turn in the main transcript (`token_watch.py` and `self-ctx.py` compute it; stale-context brief definition).
- `stale %`, self-reported: `ctx_use.stale` in `state.json`, valid when `ctx_use.at` is not older than the current `boundary_at` (a report older than the boundary is ignored).
- `stale %`, transcript proxy: logged to `local/usage/ctx-proxy.jsonl` at each evaluation for calibration; it does not trigger a clear in the first rollout. Reason: the baseline (22 sessions) shows the proxy reads 0 on 15 sessions with no detected boundary, and 99% on one session with 2 agents running. The self-report is the trigger; the proxy is the check on it.
- Fixed base: median 83k, range 71-106k; a clear cannot recover it. A clear at ctx 150k frees about 67k tokens at the median.

Signals, evaluated by `scripts/ctx-watch.py` (called by the poller each cycle, no LLM):

| Id | Signal | Source | Threshold | Debounce |
| --- | --- | --- | --- | --- |
| S1 | task done, context stale | `state.json` `ctx_use`, `boundary_at`; transcript ctx | at a task boundary: ctx >= 150k and `ctx_use.stale` >= 70 (D21 recommendation) | the same boundary fires once; needs `ctx_use.at >= boundary_at`; re-evaluate each cycle until a gate passes or a new boundary replaces it |
| S2 | context bar | transcript ctx | ctx >= 250k at the next boundary; at ctx >= 220k the signal is "approaching" and the poller asks the PM to reach a boundary | fires once per 15 min per session while true |
| S3 | idle before cache expiry | pane idle classifier (`fleet-poll.py`) and transcript last-turn time | PM at a boundary and idle (waiting on owner or Architect, no running agent) with ctx >= 150k, and idle time between 10 min and 50 min of the 60 min (3600 s) prompt-cache TTL | idle must persist 10 min; evaluated once per cycle; never fires after 50 min idle (the cache is about to expire; the next wake pays the rewrite either way, so S3 must act before) |
| S4 | restored idle history | transcript last-turn time vs session start | session idle > 60 min, ctx >= 150k, no open task in the IL session record | one evaluation per session start; baseline: 9 sessions sat at 3,331 min idle with 96-119k restored history |
| S5 | compaction guard | transcript ctx | ctx >= 400k, any time a safe gate passes (boundary not required if no task runs) | fires every cycle until the clear runs |

Why S3: a woken idle session rewrites its whole context (cache_creation) when the cache has expired, so a clear before the expiry makes the next wake cost the fixed base only. Why S4: the baseline shows sessions that restarted with large restored history and no work. Why S5: `token_watch.py` already alerts at 400k as the top ctx rung.

The floor (150k) and stale threshold (70) come from D21 and the baseline: below 150k the median session has under 67k tokens above base, and the stale-% self-report is unreliable. Both are values in `local/il-schedule.json` under `thresholds`, not constants in code. Recalibration: after 20 automatic clears, compare the self-reported stale % with the proxy and the post-clear next-turn cost; a D item proposes new values.

The Architect's own bar stays 250k (the owner 04:00Z) and uses S2, S3 and S5.

## 8. Safety gates before an automatic clear

All gates are evaluated in one command, immediately before the first send, and again before each later send. All must pass:

1. `datg.py fresh` exits 0 (IL fresh), and `written_at` is within the last 15 minutes.
2. `session.owed` is recorded with exact values or is empty by the PM's own statement (`owed_none: true`); a bare "check main" fails the gate.
3. `session.crons` is a list of every cron and Monitor the PM holds (the PM states `crons: []` when it has none). Crons and Monitors survive `/clear` (lesson, 2026-10-10), so the cycle keeps them: it deletes nothing, and the resume line tells the PM to re-create none. The harness's CronList shows only jobs created after the clear while the pre-clear jobs keep firing, so re-creating would double them; the IL keeps the ids, and a later CronDelete uses the IL id (unverified; the pilot checks it).
4. No running agent: the harness agent list is empty (`session.agents` empty and pane footer shows none); no tool call is running (`scripts/turn-active.py` false for 3 checks 5 s apart).
5. Input box `empty` or `suggestion` (`scripts/input-state.py`); `typed` fails.
6. No open dialog or permission prompt in the capture.
7. Per-session rate limit not hit (section 9).
8. The session is not on the pause list and its project is not in a `do_not_clear` set in `local/il-schedule.json`.

Failure handling: a failed gate skips the session; the reason goes to `inbox/ctx-cycle.log` and `local/usage/ctx-cycle.jsonl`; the signal retries at the next boundary or cycle. A stale or unreadable IL never clears: the poller sends a one-line request to the PM to write the IL (a normal relay with the two-step send), and clears only after a new fresh read. After 3 consecutive gate failures for one signal the poller raises an alert to the Architect and stops retrying until a new boundary.

## 9. Clear mechanism

`scripts/ctx-cycle.py <session> --signal S1|S2|S3|S4|S5 [--dry-run]`, run by the poller. Steps:

1. Gate check (section 8). Any failure: log, exit 3, send nothing.
2. Write the checkpoint (digest to a file, verified) and re-run the gates. Crons are not touched (they survive `/clear`); exit code 4 is retired and unused. Send `/clear` as a literal, verify the echo with `input-state.py`, then send Enter as a separate call (the verified two-step pattern; `scripts/relay.sh`).
4. Verify by capture that the conversation reset: ctx drops by more than 50% on the next transcript turn or the pane shows the fresh welcome state. Wait up to 30 s; timeout: log, exit 5, do not send the resume line, alert the Architect.
5. Re-run gates 4-6. Send the resume line (section 6) by the same two-step pattern.
6. Verify the resume: within 120 s the next assistant turn exists, `datg.py` was invoked (log `digest` event or a new `written_at`), and the crons still fire (CronList does not show pre-clear crons, so a script cannot compare; it records `crons_vs_cronlist: unchecked` and the Architect checks by capture in the pilot, including that a CronDelete by IL id works). Otherwise log `resume_unverified` and alert the Architect once.
7. Log one line to `local/usage/ctx-cycle.jsonl`: ts, session, signal, ctx before, ctx after (first turn), stale % reported, gates passed, exit code, seconds. Append a line to `local/actions.md` through the poller's normal alert path (not a new monitor).

Rate limit per session: at most 1 automatic clear per 60 min and 6 per day; a session that hits the limit stays uncleared and raises an alert. A resumed session that reaches S2 within 30 min of a clear is not cleared again and raises an alert (the fixed base or the restored work is too large).

Environment: tmux targets exact (`-t '=<session>:'`); the script sends no key to a pane whose session name does not match the argument; `--dry-run` runs the gates and prints the decision.

## 10. The Architect

The Architect's IL has two parts:

- Dashboard store (`local/owner-queue/`): goals, decisions, asks, tasks, session rows, as today.
- Session record: `local/il/architect.json`, the same `session` object as section 2.2, written by `datg.py --project local/il session set` (code `ar`). It holds task, next, waiting_on, owed (exact values), agents, crons and Monitors, boundary_at, ctx_use.

The Architect's handoff is derived: `scripts/architect-digest.py` prints (a) the session record, (b) active dashboard items (open D/A/T and answered-not-done), (c) session rows from the dashboard store, (d) the last 10 lines of `local/actions.md`. `scripts/self-clear.sh` sends the resume line `Resume: run python3 scripts/architect-digest.py; do not run session_context_catchup.` instead of "Resume from the Architect handoff block at the top of local/state.md".

`local/state.md` keeps the monitoring snapshot, NEXT IDS and host bands, and no per-session prose. The existing handoff block moves out; for one release the digest prints the old block's lines if present so a failed migration loses nothing.

The Architect's gates (section 8) are the existing `self-clear.sh` idle test plus: no dialog open for the owner, no relay half sent, IL fresh. The Architect clears itself from the poller via `ctx-cycle.py tm-architect`; the poller never clears the Architect while its box holds a typed draft.

## 11. Rollout

### 11.1 Install per project

Per PM (the Architect installs; a change to a project's CLAUDE.md goes to the owner first, r-067):

1. `python3 scripts/datg.py --project <dir> init --code <code>` and seed goals; migrate an existing state.json with `datg.py prune` (section 3.4) before enabling the cycle.
2. Merge the hooks block into `<dir>/.claude/settings.local.json` (SessionStart, Stop, PreToolUse Agent|Task), command `<root>/scripts/datg-hook.sh <session>`, timeout 5. A project dir shared by several PMs uses one hook entry per PM session (the script's session filter).
3. Add the PM instruction block (below) through the PM's own standing instructions, relayed once by the Architect.
4. Add the session to `local/il-schedule.json` `sessions` (name, project dir, code, thresholds override, `enabled: false` until the pilot passes).

PM instruction block (text):

> Intent Ledger. At the end of every turn, make one `datg.py batch` call that updates items and `session set` (task, next, waiting_on, owed with exact SHAs, run ids and expected main, agents, crons). At every task boundary (done, closed, merged, nothing running), end your handback with one line, exactly: `CTX-USE: active ~N% · stale ~M% · keep: <what queued work still needs>`, and put the same values in `session set --ctx-use`. N + M = 100. "stale" is context you no longer need for queued or running work: tool output, file reads and detail from finished tasks. After a clear, run the digest and continue from `next`. Your crons and Monitors survive the clear and keep firing; re-create none (CronList will not show them, the IL keeps their ids). Never write a secret value.

CTX-USE line format: `CTX-USE: active ~N% · stale ~M% · keep: <...>`; N, M integers 0-99; `keep` is under 160 characters; the separator is the middle dot with spaces.

### 11.2 Order

1. Build and test (section 12), `--dry-run` only.
2. Pilot PM: one PM with a moderate ctx and no running agents, chosen by the Architect from the baseline (tm-dogfood already has the hooks and 13 open tasks; its 3 running agents make it a poor first clear, so the candidate is a PM at a boundary such as tm-mpm-utils-01 at 211k/58% stale in the baseline). Run one full automatic cycle live: gates, checkpoint, `/clear`, resume line, verified resume. Owner sees the capture in the log. Pass bar: resume verified, no lost owed check, next-turn cost lower than the pre-clear turn.
3. Run S1 and S3 on the pilot PM for 3 days; recalibrate; then enable all PMs in groups of 3, highest ctx first, one group per day.
4. The Architect last (its own cycle moves to `ctx-cycle.py` and the IL-derived handoff).

### 11.3 Retired

- The `/tm-session-pause` snapshot step for context refreshes. Pause/resume stays for a deliberate stop of a session (park, host restart); it is not part of a refresh.
- `trusty-context-refresh` is rewritten: steps become "signals S1-S5, gates, `ctx-cycle.py`, IL digest". The pause-snapshot steps 2, 3 and 5 and the "verified checkpoint" requirement for refreshes are replaced by the IL freshness gate. Rules r-095 (crons survive clear) and r-092 (own-context measurement) stay. ARCHITECT.md r-085 and the "Context management" paragraph change to reference this spec; the hard limit "clear without a verified checkpoint" now reads "without a fresh IL" (an owner ruling is required for that text change; it goes to the owner as a D item).
- `self-clear.sh` resume text (section 10).
- The T08 "state.json < 4 KB" and DATG "2 KB" bars (replaced by section 3.3).

## 12. Build list

Sizes: S = under a day, M = 1-3 days, L = more. All are prototype Python in this repository unless stated; each is a port candidate for the Rust `trusty-architect` crate (AGENTS.md prototype rule; baked-in status goes to `local/rust-port.md`). Tests: `scripts/test_<name>.py`.

| Id | Part | Size | Acceptance tests |
| --- | --- | --- | --- |
| B1 | `datg.py` session record: `session set`, schema, limits, `batch` support | M | rejects an over-limit field (exit 2, files untouched); `session set` plus an item op is one commit; `owed` without an exact value is refused |
| B2 | `datg.py` close/prune/archive: move to `archive.jsonl` in the same atomic write; `archive-show`; title limit 120 | M | closing moves an item to exactly one file after a simulated crash between writes; `archive-show <id>` returns the item; the live trusty-tools state (a copy) prunes from 59,395 B to under 8 KB with 79 items archived |
| B3 | `datg.py rotate` and size bar (warn 8 KB, refuse 16 KB) | S | rotation moves lines older than 7 days; reader accepts both log shapes; a 17 KB state write is refused |
| B4 | `datg.py digest` and `fresh` | M | digest of the pruned live state is under 3,000 tokens; the over-budget path drops sections in order and prints TRUNCATED; `fresh` exits 1 after a state change with no write |
| B5 | hook changes: Stop hook (`stamp`), SessionStart digest injection, per-PM directory for shared projects | M | fires only for the named session; fail open on any error; stale write logs `stale_il` |
| B6 | `scripts/il-cron.py` plus `local/il-schedule.json`, wired into `fleet-poll.py` | M | overdue job runs next cycle; failing job retries 3 times then alerts; `pilot-rescore` due 2026-10-17 runs once and writes its brief |
| B7 | `scripts/ctx-watch.py`: S1-S5 evaluation and proxy logging | M | table-driven tests with fixture states and transcripts: each signal fires at the threshold and not below; debounce holds; self-report older than the boundary is ignored |
| B8 | `scripts/ctx-cycle.py`: gates, checkpoint, `/clear`, resume line, verification, log, rate limit | L | fake-tmux tests for each gate failure (nothing sent), each step's timeout, the two-step send, crons never deleted; `--dry-run` sends nothing; rate limit blocks the second clear within 60 min |
| B9 | live pilot cycle on one PM (section 11.2) | M | resume verified by capture; digest fields match the pre-clear IL; crons still fire and a CronDelete by IL id works |
| B10 | PM instruction block and `trusty-context-refresh` rewrite; ARCHITECT.md r-085 text (needs owner D item) | S | skill lint passes; no remaining step requires `/tm-session-pause` for a refresh |
| B11 | Architect IL: `local/il/architect.json`, `architect-digest.py`, `self-clear.sh` resume change, `state.md` handoff block removed | M | digest includes dashboard session rows and active items only; state.md has no per-session prose; self-clear verified by a live cycle on the Architect |
| B12 | Rollout to all live PMs (hooks, instruction relay, enable flags) | L (one group a day) | per-project: hook silent for other sessions, SessionStart shows digest, `fresh` passes after one turn |
| B13 | `tm datg` daemon verb in trusty-mpm so the per-turn write is not a Bash call (D01 follow-up) | L (trusty-tools issue, needs the owner's go) | PM writes with no Bash permission; same schema and limits |

Dependencies: B1-B5 first, then B6, B7, B8; B9 needs B1-B8; B10-B12 follow B9. B13 is not on the critical path.

## 13. Open questions

1. Self-report reliability. The trial (`2026-10-17-ctx-selfreport-trial.jsonl`) and the proxy disagree on the sessions measured; the materials cannot say which is closer. Recommended: the self-report triggers, the proxy is logged, and the thresholds are recalibrated after 20 clears (section 7).
2. First clear on a PM with running agents. Recommended: never; gate 4 holds, and S2/S5 wait for agents to end (the 250k bar is a priority, not an override of "never interrupt running agents").
3. Where a shared project's per-PM IL lives. Recommended: `datg/<session>/` under the project's `.trusty-mpm/` (section 2); the alternative, one file with a session key, makes every PM rewrite the others' data.
4. The 8 KB bar. Recommended: 8 KB (measured active set 6.8 KB). The alternative is 4 KB with 7-day task retention; it fits only if PMs keep fewer than 7 open tasks, which the pilot does not show.
5. The hard-limit wording "clear a session's context without a verified checkpoint" becomes "without a fresh IL". Recommended: change it, as a D item to the owner (policy text; ARCHITECT.md r-071 forbids the Architect from changing a policy itself).
6. S3 window (10-50 min idle). Recommended as written; the cache TTL of 3600 s is from the brief, and no measured next-wake distribution exists. Log the idle times of woken sessions for 7 days and revisit.

## 14. Resolutions (Architect, 2026-10-17 04:18Z, under the owner 04:14Z "Spec and build any needed parts")

- Q1: the self-report triggers a clear; the transcript proxy is only logged; thresholds are recalibrated after 20 clears.
- Q2: never clear with a running agent, even at the 250k bar. S5 (400k) reports to the Architect instead.
- Q3: per-PM directory `datg/<session>/` in shared project directories.
- Q4: 8 KB bar (warn 8 KB, refuse 16 KB).
- Q6: the S3 idle window stands as written; idle times of woken sessions are logged for 7 days before review.
- Q5 (hard-limit wording "fresh IL" for "verified checkpoint") and B13 (`tm datg` daemon verb in trusty-tools) go to the owner as decisions. Every automatic clear writes the IL digest to a checkpoint file and verifies it (non-empty, fresh, matches the IL) before /clear. Until the owner rules on Q5, the pilot cycle (B9) also takes a verified /tm-session-pause snapshot, so the current hard limit holds unchanged.

## 15. Ids are unique fleet-wide (the owner 2026-10-20, ar-T44)

Bare ids (D01) repeat across stores (Architect on 7890, tm-apex on 7891), so one id can name different entries. Rule:
- **Qualified form.** Anywhere an entry appears or is referenced beside another session's entries (the Architect dash, relays, `pending-answers.py`, the digest, `actions.md` lines written by scripts) it is written `<S id>:<local id>`. The S id is the session that OWNS THE STORE the id lives in, not the item's `session` field (which names the project an item concerns): the Architect store (`local/owner-queue`) is always S14, so Ddb is `S14:Ddb` even when its `session` is tm-apex; a session store is that session's S id (tm-apex store = S0c, set as `meta/state.session_id`). The qualifier is derived at render or reference time (`dashboard_qual.py`); stored ids are never renamed (r-038). A session's own dash shows bare ids.
- **No overwrite by hand-typed id.** `dashboard-db.py set` on an existing doc without `--if-version` is refused. New items use `dashboard-db.py create <collection> <prefix> --json X` (or `next <prefix>`), which computes the id in the same command.
- **Inputs.** `dashboard-db.py get|update|delete` and `owner_answers.resolve_id` accept the qualified or the bare id; a qualified id carrying another store's S id is refused with a hint naming the owning S id.
- **Nav.** The session-link strip (`local/il-dashboards.json`) is served only by the Architect dash (`--mode architect`); session dashes serve `nav: []`.
