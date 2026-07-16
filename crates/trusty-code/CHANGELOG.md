# Changelog — trusty-code

All notable changes to trusty-code are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [Unreleased]

### Added

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
  git checkout — the crates.io-tarball path. `build.rs` now also resolves
  `.git/HEAD` via `git rev-parse --git-path` (correct in a linked worktree,
  where `.git` is a file) and only emits `cargo:rerun-if-changed` for paths that
  exist, so a non-git build no longer re-runs the script on every build
  ([#2823](https://github.com/bobmatnyc/trusty-tools/issues/2823))

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
