# trusty-tools Changelog Index

This workspace is a monorepo consolidating 20 independent Rust crates. Each crate maintains its own **authoritative** changelog in its directory.

## Per-Crate Changelogs

The source of truth for changes to any crate is its own CHANGELOG:

| Crate | Changelog |
|---|---|
| **trusty-search** | [crates/trusty-search/CHANGELOG.md](crates/trusty-search/CHANGELOG.md) |
| **trusty-memory** | [crates/trusty-memory/CHANGELOG.md](crates/trusty-memory/CHANGELOG.md) |
| **trusty-analyze** | [crates/trusty-analyze/CHANGELOG.md](crates/trusty-analyze/CHANGELOG.md) |
| **trusty-mpm** | [crates/trusty-mpm/CHANGELOG.md](crates/trusty-mpm/CHANGELOG.md) |
| **trusty-agents** | [crates/trusty-agents/CHANGELOG.md](crates/trusty-agents/CHANGELOG.md) |
| **trusty-git-analytics** | [crates/trusty-git-analytics/CHANGELOG.md](crates/trusty-git-analytics/CHANGELOG.md) |
| **trusty-review** | [crates/trusty-review/CHANGELOG.md](crates/trusty-review/CHANGELOG.md) |
| **trusty-common** | [crates/trusty-common/CHANGELOG.md](crates/trusty-common/CHANGELOG.md) |
| **trusty-embedderd** | [crates/trusty-embedderd/CHANGELOG.md](crates/trusty-embedderd/CHANGELOG.md) |
| **trusty-mpm-gui** | [crates/trusty-mpm-gui/CHANGELOG.md](crates/trusty-mpm-gui/CHANGELOG.md) |

Other crates (libraries, internal tools) may not maintain per-release changelogs. Consult their git history via `git log crates/<name>/` for changes.

## Versioning

Each crate manages its own independent semantic version. When publishing, only the crates with user-facing changes are released with new versions. See [CONTRIBUTING.md](CONTRIBUTING.md) for the per-crate release convention.

## Release Tags

Releases are tagged as `<crate-name>-v<version>`. Example: `trusty-search-v0.8.0`.

To see all release tags:
```bash
git tag -l
```

To see commits for a specific crate:
```bash
git log crates/<crate-name>/
```

## Finding Changes

To see all changes across the workspace since a date:
```bash
git log --since="2026-01-01" --oneline
```

To see changes affecting a specific crate:
```bash
git log crates/<crate-name>/ --oneline
```

---

**For detailed development and release information**, see [CONTRIBUTING.md](CONTRIBUTING.md) and [docs/reference/release-workflow.md](docs/reference/release-workflow.md).

## Legacy (pre per-crate-changelog split)

The following entries document changes made before each crate adopted independent changelogs.

### [Unreleased] — 2026-05-26

#### Fixed
- **trusty-memory**: `open_activity_log_with_fallback` no longer panics on restricted filesystems — returns a `Discard` no-op log variant (#225)
- **trusty-memory**: `AppState::emit` no longer blocks the tokio runtime — activity log writes offloaded to `spawn_blocking` (#232)
- **trusty-memory**: `prompt_context_cache` uses `tokio::sync::RwLock` — KG cache rebuilds no longer stall async worker threads (#229)
- **trusty-memory**: Per-palace write mutex eliminates TOCTOU race in `dedup_gate` — concurrent identical writes now correctly deduplicate (#230)
- **trusty-mpm-tui**: Drawer detail pane and help overlay text now visible on light terminal themes — replaced `Color::White` with `Color::Reset` (#244)

#### Changed
- **trusty-memory**: `axum` and `tower-http` gated behind `axum-server` feature flag — consumers can link the rlib without the HTTP stack (`default-features = false`) (#226)
- **trusty-memory**: `dispatch_tool` refactored from 957-line monolith to 28-line router + 23 per-tool handler functions (#227)
- **trusty-memory**: Write hot path no longer triggers O(N) palace disk walks — palace name lookup uses in-memory cache; status aggregation moved to 30s background ticker (#228)
- **trusty-memory**: BM25 indexing uses bounded `mpsc::channel(256)` — burst writes no longer accumulate unbounded background tasks; queue-full events are logged and skipped (#231)

#### Internal
- **trusty-memory**: `run_serve` startup hydration deduplicated into `spawn_startup_tasks` helper (#233)
- **trusty-memory**: Test helper `test_state()` returns `(AppState, TempDir)` — eliminates `mem::forget` temp directory leaks across 262 tests (#234)
