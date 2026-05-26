# Changelog

All notable changes to trusty-tools are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Crate versions are tagged independently (`<crate-name>-v<version>`); this file
records changes in chronological order grouped by release date.

## [Unreleased] — 2026-05-26

### Fixed
- **trusty-memory**: `open_activity_log_with_fallback` no longer panics on restricted filesystems — returns a `Discard` no-op log variant (#225)
- **trusty-memory**: `AppState::emit` no longer blocks the tokio runtime — activity log writes offloaded to `spawn_blocking` (#232)
- **trusty-memory**: `prompt_context_cache` uses `tokio::sync::RwLock` — KG cache rebuilds no longer stall async worker threads (#229)
- **trusty-memory**: Per-palace write mutex eliminates TOCTOU race in `dedup_gate` — concurrent identical writes now correctly deduplicate (#230)
- **trusty-mpm-tui**: Drawer detail pane and help overlay text now visible on light terminal themes — replaced `Color::White` with `Color::Reset` (#244)

### Changed
- **trusty-memory**: `axum` and `tower-http` gated behind `axum-server` feature flag — consumers can link the rlib without the HTTP stack (`default-features = false`) (#226)
- **trusty-memory**: `dispatch_tool` refactored from 957-line monolith to 28-line router + 23 per-tool handler functions (#227)
- **trusty-memory**: Write hot path no longer triggers O(N) palace disk walks — palace name lookup uses in-memory cache; status aggregation moved to 30s background ticker (#228)
- **trusty-memory**: BM25 indexing uses bounded `mpsc::channel(256)` — burst writes no longer accumulate unbounded background tasks; queue-full events are logged and skipped (#231)

### Internal
- **trusty-memory**: `run_serve` startup hydration deduplicated into `spawn_startup_tasks` helper (#233)
- **trusty-memory**: Test helper `test_state()` returns `(AppState, TempDir)` — eliminates `mem::forget` temp directory leaks across 262 tests (#234)
