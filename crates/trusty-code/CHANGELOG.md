# Changelog — trusty-code

All notable changes to trusty-code are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [Unreleased]

### Added

- **A written log-level convention, in `logging`'s module docs.** The crate had
  no stated rule for when a decision deserves a log, which is why 16 of ~100
  source files emitted anything at all. The convention now has one organising
  principle — *a decision that silently changes the run's outcome must not be
  invisible to the operator* — and a load-bearing distinction: a **harness
  policy decision** (we overrode the model) is `warn`; a **model input error**
  (bad args, self-corrected next turn) is `debug`. Both surface identically as
  a `ToolResult::err`, which is exactly why they were conflated. Degradations
  the model or user then acts on are `info`; genuine faults are `error`
  ([#2857](https://github.com/bobmatnyc/trusty-tools/issues/2857))
- **Log-level regression guards** (`agent_loop::tests::observability`) pinning
  the LEVEL, not just the presence, of the loop's outcome-changing decisions —
  including the #2852-class acceptance test: a cap that kills a run must be
  diagnosable from ONE run's stderr. A `warn` decaying to `debug`, or an `info`
  creeping to `warn`, now both fail CI
  ([#2857](https://github.com/bobmatnyc/trusty-tools/issues/2857))

- **Build provenance in `--version` and `tcode_report.json`** — `tcode --version`
  now prints the git SHA and commit date alongside the semver
  (`tcode 0.2.0 (b20adfca 2026-07-16)`), and `tcode_report.json` carries a
  `build` object (`{version, commit, commit_date}`); the human `run-task`
  summary gains a matching `build:` header line. Previously a run's artifacts
  could not be attributed to the binary that produced them — a semver alone
  collapses every commit on a branch into one string — which during the
  2026-07-16 L4 validation forced provenance to be reverse-inferred from
  `cargo install`'s mtime reset and produced a WRONG "bug still recurs"
  conclusion, retracted only after a dedicated forensic check. Provenance is
  captured by `build.rs` from `git rev-parse --short HEAD` / `git log -1`, using
  the COMMIT date rather than a build wall-clock so rebuilds stay reproducible,
  and degrades to `"unknown"` (never a build failure, never `null`) outside a
  git checkout — the crates.io-tarball path
  ([#2823](https://github.com/bobmatnyc/trusty-tools/issues/2823))

### Fixed

- **P0: `tcode` emitted NO log output whatsoever on a default run.**
  `init_tracing` filtered with `EnvFilter::from_default_env()`, which builds an
  **empty directive set** when `RUST_LOG` is unset — enabling nothing, at any
  level. `DEFAULT_LOG_LEVEL = "info"` had documented itself as "the log level
  used when no `RUST_LOG` env var is set" since the module was written, but no
  code path ever read it: the documented default was fiction. This sits
  *underneath* every individual missing log line and made the whole class of
  invisibility bugs unfixable in principle — instrumenting a decision site
  accomplishes nothing if the subscriber discards the event, so #2852's cap was
  doubly invisible: it had no `warn!` to emit, and the default filter would have
  dropped one anyway. Both init paths now resolve `RUST_LOG` when set and valid,
  else `DEFAULT_LOG_LEVEL`; an INVALID `RUST_LOG` also falls back to the default
  (with a notice) rather than silently disabling logging, since a typo'd filter
  is the same invisibility failure triggered by the operator instead of the
  default. Verified end-to-end: a `warn!` now reaches stderr with `RUST_LOG`
  unset, and stdout stays clean for MCP JSON-RPC framing. This also removed a
  real test flake — capture-based log assertions were order-dependent because
  the empty global filter cached callsites as `Interest::never()`
  ([#2857](https://github.com/bobmatnyc/trusty-tools/issues/2857))
- **Silent control-flow: 13 decision sites that changed a run's outcome while
  emitting nothing at any level.** Three separate investigations in one day were
  caused by the harness computing something and then hiding it — the
  re-delegation cap firing silently (#2852), the discarded `CadenceOutcome`, and
  missing build provenance (#2823) — while the one mechanism that *was*
  instrumented (index-file transients at `warn`) is precisely why "27 failures
  across 23 files" was discoverable and became #2785. Logging worked exactly
  where it existed. Now instrumented per the new convention:
  - `warn` — the four terminal aborts in `agent_loop::run_inner`/`run_with_transcript`
    (**turn-cap exhaustion**, **wall-clock deadline**, and the **stop-signal**
    kill that is the *consumer* half of #2852 — its cause is logged at the cap
    site, its effect was not); the **#2279 verify gate** overruling a
    `finish_task`; the **#2683/#2805 completion-latch** refusing a delegation;
    the **#2682 redundant-run suppression** (a test run that did NOT happen);
    **RBAC denials** and **per-agent allowlist rejections** (security decisions
    that left no audit trail); the **path-traversal guard** (you could not tell
    whether an escape was ever attempted); cadence breaching its **overhead
    budget floor** (the epic #2343 ≥60%-working-context guarantee was
    unobservable); and `resolve_agent_model_slug` degrading to `"unknown"`
    (silently mispricing a run's cost aggregation).
  - `info` — degradations the model or user then acts on: the **#2783
    `STAGE_NOT_READY` → lexical fallback** (the model is served exact-match
    results instead of the semantic ones it asked for — the *failure* paths
    already warned; the *success* path was silent precisely because it
    "worked"), **`recall_session` token-budget truncation** (memories held back
    from context), and user-requested **cancellation** (deliberately not `warn`
    — the user got what they asked for).
  - `debug` — routine cadence firing, which is the mechanism working.

  `CadenceOutcome`'s own doc said it existed "for observability/tests"; its only
  call site discarded it. It is now consumed
  ([#2857](https://github.com/bobmatnyc/trusty-tools/issues/2857))
- **`build.rs` re-ran on every single build, in every tree.** Its lone
  `cargo:rerun-if-changed=.git/HEAD` directive was a relative path, which Cargo
  resolves against the *package* root — i.e. `crates/trusty-code/.git/HEAD`,
  which does not exist even in a normal checkout. Cargo treats a missing
  watched path as perpetually changed, so the script never cached. Paths are
  now resolved absolutely via `git rev-parse --git-path` (also making them
  correct in a linked worktree, where `.git` is a *file* and HEAD lives in the
  worktree's gitdir) and emitted only when they exist
  ([#2823](https://github.com/bobmatnyc/trusty-tools/issues/2823))
- **Guard against a silently stale embedded SHA when the branch ref is packed.**
  Emitting any `rerun-if-changed` opts a script out of Cargo's default
  "re-run when any package file changes" heuristic, so only declared paths are
  watched. With the branch ref packed (`git gc --auto` / `git pack-refs`) no
  loose ref file exists to watch, and a subsequent commit touches neither
  `HEAD`'s content nor any other watched path — so the script would not re-run
  and the embedded SHA/date would go stale until a `cargo clean`, which is the
  very failure this ticket exists to eliminate. The append-only reflog
  (`logs/HEAD`) is now watched as well: it is touched by effectively every
  ref-changing operation regardless of whether the ref is packed or loose,
  closing the gap without reverting to an unconditional re-run
  ([#2823](https://github.com/bobmatnyc/trusty-tools/issues/2823))

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
