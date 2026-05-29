# Architecture

Technical reference for the `trusty-git-analytics` (`tga`) single-crate Rust implementation.

---

## Single-Crate Overview

`tga` is a single Rust crate (`tga`) that exposes both a reusable library (`tga::*`) and
the `tga` binary. All analytical logic lives in four library modules. The binary entry
point and its subcommand handlers are compiled only as part of the binary and are not
part of the public API.

This is a consolidation from an earlier five-crate workspace design (`tga-core`,
`tga-collect`, `tga-classify`, `tga-report`, `tga-cli`). The single-crate layout
eliminates inter-crate dependency overhead, simplifies the build graph, and allows
integration tests to reference all library code without a dependency on the CLI crate.

---

## Module Map

```
src/
├── main.rs                     # Binary entry point  (#[tokio::main])
├── commands/                   # Binary-private subcommand handlers
│   ├── analyze.rs
│   ├── collect.rs
│   ├── classify.rs
│   ├── report.rs
│   ├── pr_metrics.rs
│   ├── aliases.rs
│   ├── backfill.rs
│   ├── override_.rs
│   └── install.rs
├── core/                       # tga::core — foundation, no internal deps
│   ├── config/                 #   YAML config structs (serde_yaml)
│   ├── db/                     #   rusqlite wrapper + migration runner
│   │   └── sql/                #   Versioned SQL migration files (include_str!)
│   ├── errors.rs               #   TgaError enum (thiserror)
│   └── models/                 #   Shared domain structs
├── collect/                    # tga::collect — Stage 1
│   ├── git/                    #   git2 revwalk and diff extraction
│   ├── github/                 #   GitHub REST client (reqwest + tokio)
│   ├── bitbucket/              #   Bitbucket REST client
│   ├── jira/                   #   JIRA REST client
│   ├── linear/                 #   Linear GraphQL client
│   ├── azdo/                   #   Azure DevOps REST client
│   ├── identity/               #   IdentityResolver (exact + Jaro-Winkler fuzzy)
│   ├── pm_adapter.rs           #   PmAdapter trait
│   ├── pr_provider.rs          #   PrProvider trait
│   └── collector.rs            #   CollectionPipeline
├── classify/                   # tga::classify — Stage 2
│   ├── tiers/                  #   exact, regex, fuzzy, llm
│   ├── rules/                  #   Rule types, YAML loader, 80+ default rules
│   ├── pipeline.rs             #   ClassificationPipeline
│   └── classifier.rs          #   ClassificationEngine
└── report/                     # tga::report — Stage 3
    ├── formatters/             #   csv, json, markdown writers
    ├── aggregator.rs           #   SQL aggregation queries
    ├── models.rs               #   Report-level structs
    └── pipeline.rs             #   ReportPipeline
```

---

## Module Dependency Diagram

```
tga::core  (no internal deps)
     │
     ├──► tga::collect   ──────────────────────────────┐
     │                                                  │
     ├──► tga::classify  ──────────────────────────────┤
     │                                                  │
     └──► tga::report    ──────────────────────────────►  src/main.rs
                                                           src/commands/
```

`tga::core` is the only module imported by all others. `collect`, `classify`, and
`report` do not depend on each other — they communicate only through the shared SQLite
database. `src/commands/` depends on all four library modules and is compiled only into
the binary.

---

## Binary vs. Library Boundary

The library boundary is at the four `tga::*` modules. The binary-private boundary
encompasses `src/main.rs` and `src/commands/`.

| Scope | Contents |
|---|---|
| Public library (`tga::*`) | `tga::core`, `tga::collect`, `tga::classify`, `tga::report` |
| Binary-private | `src/main.rs`, `src/commands/` |

Integration tests link against the library (no dependency on the binary) and can invoke
`CollectionPipeline`, `ClassificationPipeline`, and `ReportPipeline` directly.

---

## Key Crate Dependencies

| Crate | Why chosen |
|---|---|
| `git2` | libgit2 Rust bindings. Statically linked. No `git` subprocess; enables fast revwalk without fork overhead and works in read-only environments. |
| `rusqlite` (bundled) | SQLite with the `bundled` feature — no system SQLite required. Single-writer, WAL mode, zero-copy reads via `mmap`. |
| `tokio` | Async runtime for all HTTP clients and LLM calls. `#[tokio::main]` in `src/main.rs`. |
| `rayon` | Data parallelism for batch commit classification (Tiers 0–2). Work-stealing thread pool; no GIL equivalent. |
| `clap` | CLI with derive macros. Same subcommand structure and flag names as the Python predecessor wherever possible. |
| `serde` + `serde_yaml` | Config deserialization. `#[serde(default)]` on all fields means unknown keys and missing optionals never error. |
| `aho-corasick` | Multi-pattern substring search for Tier 1 exact keyword matching. O(n) scan of commit message regardless of keyword count. |
| `strsim` | Jaro-Winkler fuzzy matching for identity resolution. |
| `chrono` | Date/time with ISO week support (`IsoWeek`). |
| `tera` | Jinja2-compatible template engine for Markdown report rendering. |
| `blake3` | Fast cryptographic hashing for config file change detection. |
| `anyhow` | Error handling in the binary (`src/main.rs`, `src/commands/`). |
| `thiserror` | Typed error enums in library modules (`tga::core`, etc.). |
| `reqwest` | Async HTTP client for GitHub, Bitbucket, JIRA, Linear, and ADO APIs. Uses `rustls` — no system OpenSSL. |

---

## Three-Stage Pipeline Flow

```
 git repository (local clone)
         │
         │  git2 revwalk
         ▼
 ┌───────────────────────────────────────────────┐
 │  Stage 1: Collect                             │
 │                                               │
 │  IdentityResolver: (name, email) → canonical  │
 │  PrProvider impls: fetch PR metadata          │
 │  PmAdapter impls: fetch ticket data           │
 │                                               │
 │  Writes: commits, authors, pull_requests,     │
 │          work_items, collection_runs          │
 └──────────────────────┬────────────────────────┘
                        │  SQLite (tga.db)
                        ▼
 ┌───────────────────────────────────────────────┐
 │  Stage 2: Classify                            │
 │                                               │
 │  Tier 0: classification_overrides table       │
 │  Tier 1: Aho-Corasick exact keyword match     │  ──► rayon::par_iter
 │  Tier 2: regex match                          │  ──► rayon::par_iter
 │  Tier 2.5: fuzzy/structural heuristics        │  ──► rayon::par_iter
 │  Tier 3: LLM fallback (tokio, async)          │
 │                                               │
 │  Writes: classifications (FK to commits)      │
 └──────────────────────┬────────────────────────┘
                        │  SQLite (tga.db)
                        ▼
 ┌───────────────────────────────────────────────┐
 │  Stage 3: Report                              │
 │                                               │
 │  SQL aggregation queries (aggregator.rs)      │
 │  CSV formatter  → 9 .csv files                │
 │  JSON formatter → 4 .json files               │
 │  Markdown formatter → 1 report.md             │
 └──────────────────────┬────────────────────────┘
                        │
                        ▼
                   ./reports/
```

---

## Database Design Rationale

### Single file, single process

`tga` uses a single SQLite database file (`tga.db` by default). All three pipeline
stages read from and write to the same file. There is no separate identities database.

SQLite in WAL mode supports concurrent reads during write-heavy collection while
guaranteeing single-writer safety. For `tga`'s usage pattern (one writer, occasional
readers such as direct SQL queries during development), WAL is the optimal journal mode.

```sql
-- Applied on every Database::open()
PRAGMA journal_mode = WAL;
PRAGMA synchronous  = NORMAL;
PRAGMA foreign_keys = ON;
```

### Migration-based schema

The schema is managed by a versioned migration runner. SQL files are embedded at compile
time (`include_str!`) and applied in order on `Database::open()`. Each migration runs
inside a transaction that also inserts the version record — partial migration is
impossible.

This approach means:
- A new binary automatically upgrades an older database on first run.
- Developers add new tables or columns by appending a new migration file.
- Existing migrations are never edited — they represent applied history.

### Per-week collection tracking

The `collection_runs` table records each `(repo_name, iso_year, iso_week)` tuple that
has been successfully collected. This is the foundation of incremental collection:
`tga collect --weeks 1` skips weeks that already have a row, making it safe to run as a
weekly cron job without re-processing historical data. `--force` bypasses this check.

---

## Async / Sync Split

| Concern | Mechanism |
|---|---|
| HTTP (GitHub, JIRA, Linear, ADO, LLM) | `tokio` async |
| git extraction (libgit2) | Synchronous; blocking call inside async context |
| Classification batch (Tiers 0–2) | `rayon` parallel; called from sync context within async pipeline |
| LLM fallback (Tier 3) | `tokio` async; awaited after Rayon batch completes |
| CSV / JSON / Markdown write | Synchronous |

`src/main.rs` runs a `#[tokio::main]` entry point. `CollectionPipeline::run()` and
`ClassificationPipeline::run()` are `async` to accommodate HTTP and LLM calls. The git
revwalk and Rayon batch classification inside those pipelines are synchronous blocking
calls that complete before the async methods return.

`git2` repository handles are not `Send + Sync`. They are opened and dropped within the
per-repo processing block and are never shared across threads.

---

## Error Handling Strategy

Library modules (`tga::core`, `tga::collect`, `tga::classify`, `tga::report`) use
`thiserror` to define typed error enums:

- `tga::core::TgaError`
- `tga::collect::CollectError`
- `tga::classify::ClassifyError`
- `tga::report::ReportError`

Each module exposes a `Result<T>` type alias scoped to its own error type.

The binary (`src/main.rs`, `src/commands/`) uses `anyhow::Result`. The `?` operator
converts from any library error via its `std::error::Error` implementation. This keeps
library error types precise and composable while giving the binary readable error chains.

`unwrap()` and `expect()` are forbidden in library code. Test code uses `expect()` with
descriptive messages.
