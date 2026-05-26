# SESSION-2026-05-26 — trusty-memory Code Analysis and Fixes

## Overview

A systematic code analysis pass over `crates/trusty-memory/` identified 10
correctness, safety, and performance issues. Each was filed as a GitHub issue
(#225–#234), implemented in five focused PRs (#239–#243), and merged to main
on 2026-05-26.

The pass covered async correctness (blocking calls on the tokio runtime), race
conditions in the write deduplication gate, O(N) disk walks in the write hot
path, unbounded background task accumulation, and test infrastructure leaks.

---

## Issues Found and Resolved

| # | Title | Severity | Fix Summary |
|---|---|---|---|
| #225 | `ActivityLog` panics on restricted filesystems | High | Converted `ActivityLog` to an enum with `Redb` and `Discard` variants; `open_activity_log_with_fallback` returns `Discard` when the storage path is unwritable |
| #226 | `axum` unconditional dep breaks rlib consumers | Medium | Gated `axum` and `tower-http` behind `axum-server` feature flag (default enabled); `open-mpm` now links without the HTTP stack |
| #227 | `dispatch_tool` is a 957-line monolith | Medium | Decomposed into 28-line router + 23 per-tool `handle_*` functions; shared write pipeline unified in `write_drawer(state, WriteDrawerParams)` |
| #228 | Palace name lookup does an O(N) disk walk per write | High | Replaced filesystem walk with an in-memory `DashMap` cache populated at startup; `aggregate_status_event` removed from the write path, replaced by 30-second background ticker |
| #229 | `prompt_context_cache` uses `std::sync::RwLock` | Medium | Replaced with `tokio::sync::RwLock` — KG cache rebuilds no longer stall async worker threads |
| #230 | TOCTOU race in `dedup_gate` under concurrent writes | High | Added per-palace write mutex; concurrent identical writes now correctly deduplicate instead of both passing the gate |
| #231 | BM25 indexing spawns unbounded background tasks | Medium | Replaced unbounded per-write spawns with bounded `mpsc::channel(256)` + single background worker; queue-full events log a warning and skip (best-effort) |
| #232 | `AppState::emit` blocks the tokio runtime | High | Offloaded redb activity log write to `spawn_blocking` |
| #233 | Duplicated startup hydration in `run_serve` | Low | Extracted into `spawn_startup_tasks` helper |
| #234 | `test_state()` leaks TempDir via `mem::forget` | Low | `test_state()` now returns `(AppState, TempDir)` — caller holds the guard, which drops correctly at end of test scope |

---

## Architecture Changes

### axum Feature Flag (#226)

`axum` and `tower-http` are now optional, gated behind the `axum-server`
feature (default enabled). This aligns `trusty-memory` with the workspace
rule requiring HTTP stacks to be feature-flagged in rlib crates. `open-mpm`
can now link `trusty-memory` without pulling in axum.

See `docs/trusty-memory/research/axum-feature-flag-decision-2026-05-26.md`
for the full decision record.

### dispatch_tool Decomposition (#227)

The original `dispatch_tool` function had grown to 957 lines through
incremental feature additions, making it difficult to test individual tool
handlers in isolation. The refactor introduces a 28-line routing match
statement that delegates to 23 per-tool `handle_*` functions. The shared
write pipeline (`memory_remember` + `memory_note`) is unified in a single
`write_drawer(state, WriteDrawerParams)` call, eliminating the duplicated
validation and deduplication logic that existed in each branch.

### Write Hot-Path Performance (#228, #231)

Two independent optimisations targeted the write path:

1. **Palace name resolution**: The previous implementation walked the
   filesystem on every write to map a palace ID to its display name. This
   has been replaced by a `DashMap` populated during daemon startup.
   `aggregate_status_event` was also removed from the synchronous write
   path and replaced by a 30-second background ticker.

2. **BM25 indexing backpressure**: Each write previously spawned an
   independent tokio task to update the BM25 index, which meant burst
   writes could accumulate an unbounded queue of background work. A bounded
   `mpsc::channel(256)` now feeds a single background worker. When the
   channel is full, the write completes but the BM25 update is skipped with
   a `WARN`-level log entry (best-effort semantics; vector recall is
   unaffected).

---

## Test Coverage

New tests added as part of this pass:

- **Discard fallback** (#225): test that `open_activity_log_with_fallback`
  returns `Discard` when given an unwritable path, and that `Discard` writes
  silently succeed without panicking
- **TOCTOU race regression** (#230): concurrent-write test that submits 50
  identical texts from 50 parallel tasks and asserts exactly one drawer is
  created (verifies per-palace mutex holds)
- **BM25 queue-drop under load** (#231): flood test that sends 512 writes
  over a `channel(256)` and asserts the process does not panic and the
  warning counter increments
- **Palace name cache** (#228): test that `lookup_palace_name` returns
  correct results after cache population and that a cache miss falls back
  correctly

The `test_state()` change (#234) fixed latent TempDir leaks across the
existing 262-test suite; no tests were deleted.

---

## Commits

| PR | Commit | Description |
|---|---|---|
| #239 | `c0dfc9d` | safety + async correctness: ActivityLog enum, spawn_blocking, tokio RwLock, per-palace mutex |
| #240 | `ab8ebbc` | axum feature flag: gate axum/tower-http behind axum-server feature |
| #241 | `ce11eaa` | dispatch_tool refactor: 957-line monolith → 28-line router + 23 handlers |
| #242 | `41411a1` | write hot-path perf: DashMap palace cache, background ticker, bounded BM25 channel |
| #243 | `feb5a79` | cleanup: spawn_startup_tasks helper, test_state returns TempDir |
