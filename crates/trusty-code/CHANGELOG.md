# Changelog — trusty-code

All notable changes to trusty-code are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [Unreleased]

### Changed

- **Deliverable-completeness guidance in the `BASE` preamble (bumped to
  `1.8.0`)** — makes completion of a task's full required-artifact SET robust to
  turn-budget variance on large tasks. On the bake-off L4 task, completion was
  nondeterministic across identical runs: run-2's engineer consumed its entire
  40-turn budget (transcript turns 9..=48) and opened its FINAL turn with "Now
  let me create the README and ARCHITECTURE files", so `ARCHITECTURE.md` was
  never written — while run-3 finished the same task in 29 turns. The provided
  suite passed 9/9 in both, so the code was never the problem: documentation was
  queued LAST, behind ~130 self-authored tests, which made it the only
  deliverable exposed to turn-budget variance. The preamble now tells the model
  to enumerate the required-artifact set as a checklist up front and track what
  remains, treats task-named documentation as a deliverable rather than an
  epilogue, orders required documents at the point the design settles (once the
  code exists and the project's own suite passes) and ahead of any discretionary
  work, forbids starting discretionary work while a required artifact is still
  missing, scales self-authored tests to the stated requirements, points the
  multi-document tail at the `write_files` batch tool (run-2 spent 19 of 40
  turns on one-file-per-turn writes), and requires a final checklist sweep
  before finishing. No turn-budget change: run-2 spent MORE turns than the runs
  that completed, so the budget was not the binding constraint. (#2824)

---

## [0.2.0] — 2026-07-16

### Added

- Surface trusty-search index readiness to daily-driver task sessions: after warming the project's index at task start, `ensure_project_indexed_in_background` now probes `GET /indexes/{id}/status` (via the shared `trusty_common::search_readiness::log_index_readiness`) and emits one stderr line describing which lanes are ready — `warn` while semantic embedding is still warming (expect lexical-only results), `info` once it is ready — so a session is never silently querying a not-yet-ready index ([#2784](https://github.com/bobmatnyc/trusty-tools/issues/2784))
- **Redundant full-suite test re-run suppression (`redundant_run`)** — the
  complement to the `verify_gate` (#2279): where that guarantees the suite runs
  at least once, this stops it running more than needed. When an identical
  `bash` test command already passed and no code has changed since (no
  `write_file`/`write_files`/`edit` and no other shell command in between), the
  agent loop short-circuits the re-run with an explanatory sentinel result
  instead of spawning the suite, so the delegated engineer stops burning turns
  on repeated "one final run to confirm" passes. Opt-in via
  `AgentLoop::with_redundant_run_suppression`, attached at the single
  delegated-engineer construction site alongside the verify gate. The `BASE`
  preamble additionally instructs the model that one clean run after the last
  code change is sufficient and not to repeat identical confirmation runs
  (preamble bumped to `1.7.0`). (#2682)

---

### Changed

- `search_code`: when the semantic (or symbol) lane returns `STAGE_NOT_READY`
  because the vector/KG index stage is still building on a freshly-indexed repo,
  the tool now transparently retries via the always-available lexical lane and
  returns those (degraded) hits instead of an empty "use grep/glob" fallback —
  so conceptual discovery still works during the embedding warm-up window
  (#2783).

### Fixed

- Bound PM re-delegation so a degenerate delegate loop cannot mislabel a
  complete run. Once a delegated engineer reports an explicit successful
  `finish_task` completion, the PM's `delegate_to_agent` tool refuses any
  further re-delegation (nudging it to `finish_task` instead), and
  `run_task`'s report assembler now reports `success`/`no_changes` — never
  `partial`/exit-6 or `deadline_exceeded` — for a run the engineer already
  completed, regardless of how the PM's own loop later terminated. This closes
  the data-integrity bug where a gratuitous post-`finish_task` re-verify round
  that ran out of turns/time corrupted run status and telemetry on a
  fully-passing, all-tests-green run (#2683).

---

## [0.1.0] — 2026-07-09

### Added

- Initial crates.io release.
