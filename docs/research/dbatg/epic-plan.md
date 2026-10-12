# Every tm-launched PM keeps a structured plan the Architect ingests without a manual step

## Tracker

**Title (created as `[EPIC] ...`, renamed after the number is known):**
`[EPIC <E>] Every tm-launched PM keeps a structured plan the Architect ingests without a manual step`

**Labels:** `epic`, `trusty-mpm`, `daemon`, `ws/tm-architect`.
**Milestone:** `trusty-mpm 2.0.0` (milestone 97, due 2026-11-27). **Project:** #45. **Parent:** #8536 (native sub-issue). **Assignee:** `@me`.

## Epic plan

Today a PM's goals, decisions, asks and tasks live in a Python prototype on two PMs, with no Briefs, no tm deployment and no guaranteed delivery to the Architect. When this is done, every tm-launched PM keeps its own plan (goals, decisions, asks, tasks, briefs) as structured JSON, never committed, written only by that PM, and the Architect ingests it deterministically.

Runs after the `trusty-architect` crate (#9171) exists. Phase 1 starts when the #9171 crate is merged to main (Architect call 03:37Z).

### Outcomes

- **O1** Two PMs in one checkout write separate stores; a duplicate code is refused.
- **O2** A PM that cannot run shell writes its plan through an MCP tool or daemon method.
- **O3** A PM-written brief appears in the Architect dashboard Briefs section.
- **O4** Killing the Architect, the console or the daemon mid-flow loses no item; replays create no duplicate.
- **O5** `.trusty-mpm/dbatg/` is ignored in a fresh project, and a secret-shaped value is refused on write.
- **O6** A new PM receives the DBATG rules and hooks from tm with no project edit.
- **O7** The Architect settles only items covered by an existing ruling or the routine class; all others reach the owner.
- **O8** The Python prototype (`datg.py`, `pm_intake.py`, `pm-notify.sh`, `pm-digest.py`) is deleted from the Architect project.

### Ratified decisions

| # | Decision |
|---|---|
| D1 | Release: DBATG ships in 2.0.0 as this epic, after #9171. |
| D2 | Storage: per-PM files are the source of truth. The console bus carries only a `dbatg.changed` notification. |
| D3 | Granularity: one store per session, `<project>/.trusty-mpm/dbatg/<session>/`, with a per-host code registry. |
| D4 | Write path: daemon method plus MCP tool; `tm dbatg` is the CLI wrapper. |
| D5 | Briefs: the PM writes them to its store; the Architect shows the paragraph in the dashboard Briefs section. |
| D6 | The Architect settles alone only items answered by an existing ruling or in the routine class. |
| D7 | Pilot exit: extend the T08 pilot one cycle with escalation wired and warns logged, then port. |
| D8 | Scope: single host, single owner. Cross-host goes through the daemon method later and is out of scope. |

### Ordering

The store and its writer come first: nothing else has data to carry. The outbox and ingest (phase 4) must be proven by replay and kill tests before the bus notification (phase 5) and the Architect settle rule (phase 8) rely on them. Hooks (phase 7) deploy only after the writer works without shell, because the dogfood PM rule P10 forbids the per-turn Bash call. The pilot exit and Python removal (phase 9) are last: they need a live two-PM proof and a held-steady cycle.

<!-- phases:start -->
| # | Phase | Issue | State | Gate |
|---|-------|-------|-------|------|
| 1 | Schema, ids, per-session store and code registry | #<n1> | open | #9171 crate merged |
| 2 | Writer: daemon method, MCP tool, `tm dbatg` CLI | #<n2> | open | PHASE_1 merged |
| 3 | Briefs kind and dashboard Briefs | #<n3> | open | PHASE_4 merged |
| 4 | Outbox, seq, ack and idempotent ingest | #<n4> | open | PHASE_2 merged |
| 5 | Directory watcher and `dbatg.changed` bus event | #<n5> | open | PHASE_4 proven; console bus slices |
| 6 | Scaffold ignore rule | #<n6> | open | PHASE_1 merged |
| 7 | PM instruction section and tm-generated hooks | #<n7> | open | PHASE_2 merged |
| 8 | Architect-side session choice and settle rule | #<n8> | open | PHASE_4 merged |
| 9 | Live proof, pilot exit and Python removal | #<n9> | open | PHASES 5, 7, 8 merged |
<!-- phases:end -->

### Phases

Common to all phases: title form `[EPIC_<E> PHASE_<n>] <what>`; native sub-issue of the tracker (`--parent <E>`); milestone `trusty-mpm 2.0.0`; project #45; `ws/tm-architect`; assignee `@me`; body ends with the attribution line. Each body uses the five headings (Scope, Acceptance criteria, Non-goals, Risk, Gate) from the phase template; the 2-4 line summary below is the opening paragraph. Rung is the review's R0-R4 scale (section 5) and sets the close bar: R1 closes at Merged, R2-R4 close at Deployed with live evidence.

Review failure-mode numbers (F1-F15) refer to review section 3.

### Phase: Schema, ids, per-session store and code registry (Q3)

Gate: #9171 crate merged

Labels: `enhancement`, `trusty-mpm`. Rung R1. Depends on: #9171 crate merged.

Defines schema v1 for G/D/A/T/B in the `trusty-architect` crate, with a JSON Schema file, ids `<code>-<Prefix><2 hex>`, and the layout `<project>/.trusty-mpm/dbatg/<session>/` (`state.json`, `log.jsonl`, `outbox.jsonl`, `acks.json`, `.lock`). Adds the per-host code registry. The store anchors to the session's launch directory, not the worktree cwd (F1, F2, F5, F8).

#### Acceptance criteria

- **AC1** Fixtures written by the Python `datg.py` read back through the Rust types and re-serialize without losing a field.
- **AC2** Two sessions in one checkout (a test with two tmpdir sessions sharing a project path) produce two stores with independent `next` counters. A shared single store fails this.
- **AC3** `init` with a code already registered to another live session exits non-zero and writes nothing.
- **AC4** A writer on schema N refuses a store with `schema` N+1 and leaves it byte-identical; a reader ignores unknown keys.
- **AC5** Run from a worktree cwd, the store path resolves to the launch directory.

### Phase: Writer: daemon method, MCP tool and `tm dbatg` CLI (Q4)

Gate: PHASE_1 merged

Labels: `enhancement`, `trusty-mpm`, `daemon`, `mcp`. Rung R2. Depends on: PHASE_1.

Adds the verbs init, add, update, close, batch (`--file` and stdin), prune and show, as one daemon method with an MCP tool, and the `tm dbatg` CLI as a wrapper. The writer takes the flock, validates, appends the outbox and log line first, then replaces the snapshot, then fsyncs (F3). It runs a secret-shape check and refuses (F11). It prunes done items older than 7 days into `log.jsonl` after each write (F7).

#### Acceptance criteria

- **AC1** All 21 `test_datg.py` cases are ported and green against the Rust writer.
- **AC2** A write containing a secret-shaped string exits 2, leaves all files untouched, and the log line records the refusal without the value.
- **AC3** A process killed between the log append and the snapshot replace recovers on next start by replaying lines above the snapshot's `v`; the item is present exactly once.
- **AC4** A PM session that cannot run shell writes an item through the MCP tool, live, on this host.
- **AC5** A done item aged 8 days is absent from `state.json` and present in `log.jsonl`; the things-sized fixture stays under the 2 KB hook target for its derived line.
- **AC6** A read-modify-write with a changed `v` logs `tamper` and does not overwrite.

### Phase: Briefs kind and dashboard Briefs (Q5)

Gate: PHASE_4 merged

Labels: `enhancement`, `trusty-mpm`, `ui`. Rung R1/R3. Depends on: PHASE_4 (ingest carries the brief).

Adds kind B (id prefix, one-paragraph `answer`, optional `doc`, `goal`, status) to the writer and the ingest, and renders ingested briefs in the dashboard Briefs section with the `doc` link.

#### Acceptance criteria

- **AC1** A brief written by a PM appears in the Briefs section with its paragraph and `doc` link, within one ingest cycle.
- **AC2** A brief over the one-paragraph cap is refused at write time, not truncated.
- **AC3** A brief does not wake the Architect; it shows as status only.
- **AC4** Re-ingesting the same brief revision creates no second dashboard entry.

### Phase: Outbox, seq, ack and idempotent ingest (Q2, failure modes)

Gate: PHASE_2 merged

Labels: `enhancement`, `trusty-mpm`. Rung R1/R2. Depends on: PHASE_2; the D10 message-intake port.

Ingest reads `outbox.jsonl` lines above `acked_seq`, keyed `(code, id)`, applying a line only if `rev` is higher. A gap in `seq` triggers a resync from `state.json`. The ingest writes `acks.json`; the PM sees `UNACKED` after N turns. The ingest keeps no in-process state (F4, F5, F12).

#### Acceptance criteria

- **AC1** Replaying a line, reordering two lines, and duplicating a line each leave one item per `(code, id)`.
- **AC2** With the Architect-side ingest killed and restarted, no queued line is lost and `acked_seq` advances to the last line.
- **AC3** A forced `seq` gap logs the gap and rebuilds from `state.json`; the result equals the snapshot.
- **AC4** A changed `epoch` (store re-initialised) is detected and not merged into the old copy.
- **AC5** An un-acked item shows `UNACKED` in the PM's status line and in the dashboard feed as a per-session count.
- **AC6** The Architect process never writes any file in the PM store except its own `acks.json`.

### Phase: Directory watcher and `dbatg.changed` bus event (Q2)

Gate: PHASE_4 proven; console bus slices

Labels: `enhancement`, `trusty-mpm`, `trusty-console`, `daemon`. Rung R2. Depends on: PHASE_4 proven; the 1.9.0 console bus slices.

The tm daemon watches the `dbatg/` directories of sessions it launched and, after a successful write and outbox append, emits one `dbatg.changed` event (session, `seq`) to the console bus. The 600 s poll stays as the fallback. The event never carries item content.

#### Acceptance criteria

- **AC1** A PM write wakes the ingest within 10 s with the console running.
- **AC2** With the console stopped, the 600 s poll still ingests the same write. A bus-only path fails this.
- **AC3** The event payload contains no free text (asserted on the frame).
- **AC4** A dropped event (queue-full test) costs latency only; no item is lost.
- **AC5** The watcher handles a `dbatg/` directory created after the daemon started. Verify this first: the review did not confirm the daemon can watch directories it did not create (review caveat 3).

### Phase: Scaffold ignore rule for `.trusty-mpm/dbatg/`

Gate: PHASE_1 merged

Labels: `enhancement`, `trusty-mpm`. Rung R3. Depends on: PHASE_1.

`tm` writes `.trusty-mpm/dbatg/` into the managed ignore block on first init (today it ignores only `sessions/`, `logs/`, `last-instructions.md`).

#### Acceptance criteria

- **AC1** In a fresh project with no broad `.trusty-mpm/` rule, after `tm dbatg init`, `git status` shows nothing from `dbatg/` and `git check-ignore` matches.
- **AC2** Running init twice leaves one rule line.
- **AC3** An existing user `.gitignore` content outside the managed block is untouched.

### Phase: PM instruction section and tm-generated hooks

Gate: PHASE_2 merged

Labels: `enhancement`, `trusty-mpm`, `daemon`. Rung R3. Depends on: PHASE_2.

Adds the DBATG rules (about 160 tokens) as a section of the PM instruction package, deployed by tm, and generates the hooks: SessionStart (rules and goals), UserPromptSubmit (one status line), PreToolUse Agent (goal id check), Stop (write reminder when `v` did not move). Replaces the per-project `settings.local.json` relay. All hooks fail open; block mode stays off (F6, F13, F14).

#### Acceptance criteria

- **AC1** A newly launched PM in a project with no DBATG files receives the rules and the hooks with no project edit.
- **AC2** Each hook, with the store unreadable or the daemon down, exits 0 and emits nothing harmful.
- **AC3** Hooks plus write cost at most 150 tokens per turn, measured on a recorded session.
- **AC4** Hooks emit a derived line and never read `state.json` whole into context.
- **AC5** A PM session whose `v` did not move in the turn gets the Stop reminder; one that wrote does not.

### Phase: Architect-side session choice and settle rule (Q6)

Gate: PHASE_4 merged

Labels: `enhancement`, `trusty-mpm`. Rung R1/R3. Depends on: PHASE_4.

The Architect proposes start, pace or park for sessions from ingested goals and tasks (stop and park stay owner decisions). For each ingested D or A it recalls rulings, answers by relay only for an item covered by an existing ruling or the routine class, and passes the rest to the owner as a dashboard question with the PM's options and markers. Non-matching items are never settled alone.

#### Acceptance criteria

- **AC1** An item matching a stored ruling is answered by relay and marked done with a verified outcome.
- **AC2** An item with no matching ruling, rated High confidence by the Architect, still goes to the owner (the shortcut D6 option (c) would allow). A test item proves it.
- **AC3** Escalated items keep the PM's `markers` and `specs`.
- **AC4** A PM whose `v` has not moved while its pane shows work is marked `stale` on the dashboard; no ruling is derived from it.

### Phase: Live proof, pilot exit and Python prototype removal (Q7)

Gate: PHASES 5, 7, 8 merged

Labels: `chore`, `trusty-mpm`. Rung R4. Depends on: PHASES 5, 7, 8 merged.

Extends the T08 pilot one cycle with escalation wired and warns logged, then runs the live proof: two PMs, one machine restart. When the Rust version is released and installed on this host and live-checked against the Python output, deletes `datg.py`, `pm_intake.py`, `pm-notify.sh`, `pm-digest.py`, their tests and their `scripts/README.md` lines, and points every caller (skills, poller, hooks, dashboard) at the daemon (AGENTS.md "Prototype installation"). Tracks the rows in `local/rust-port.md`.

#### Acceptance criteria

- **AC1** After a PM restart and an Architect restart, all items survive, the un-acked count returns to 0, and the dashboard has no duplicate item.
- **AC2** The Rust and Python outputs match on the same fixture store before the Python is deleted; the diff is attached.
- **AC3** After deletion, `grep` of the Architect project for the four script names finds no caller.
- **AC4** The pilot score at exit shows escalation messages carrying a DATG id (the 03:20Z baseline was 0 of 25).
- **AC5** `local/rust-port.md` shows each ported script as removed.

### Deferred

<!-- deferred:start -->
| Item | Why deferred | Where it went |
|------|--------------|---------------|
| Multi-host and multi-owner stores | D8: single host, single owner in 2.0 | unscheduled |
| Dispatch-hook block mode | Not baked: no block run yet | stays in the Python prototype; unscheduled |
<!-- deferred:end -->

## Follow-ups

<!-- followups:start -->
| Finding | Surfaced by | Severity | Where it went |
|---------|-------------|----------|---------------|
<!-- followups:end -->

## Maintenance

The phases block is regenerated from child issue state, never patched by hand. The deferred block is amended deliberately. The follow-ups block is appended as findings surface; at most two per phase become standalone issues, and only at severity HIGH or above. The sections above the markers are authored once.

Comments here are limited to changes of plan.

## Open questions

1. The review is not on `origin/main` of trusty-tools. `tm issue epic create` refuses until it is. Who lands it as `docs/research/dbatg/design-review.md` (the Architect cannot code; this is a docs PR via an engineer or the owning PM)?
2. The `trusty-architect` crate label is absent from TICKETING.md's crate list. OK to create it after `gh label list`, or use `trusty-mpm` only?
3. Phase 5 assumes the daemon can watch directories it did not create (review caveat 3). Unverified. Phase 5 AC5 tests it first; if it fails, the fallback is the 600 s poll only.
4. Phase 5 depends on the 1.9.0 console bus slices. Not read (review caveat 2). The 2.0.0 date (2026-11-27) may need that dependency checked before filing.
5. The review's Q1 (a) said "built after the crate lands". Phase 1 gate is "#9171 compiles", not "#9171 closed". Confirm that reading.
