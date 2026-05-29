# Rust Architecture Decisions

This document records the Rust-specific design decisions for `trusty-git-analytics` and
the reasoning behind them.

## Crate Structure

The project is a single `tga` crate (consolidated from the original 5-crate workspace in v1.0.0). The library and binary are both built from this crate.

| Module | Path | Responsibility |
|--------|------|----------------|
| `tga::core` | `src/core/` | Shared types, config schema (serde), DB schema + migrations (rusqlite), error types, identity model, `ChangeType` enum |
| `tga::collect` | `src/collect/` | Stage 1: git extraction (git2), GitHub/JIRA/Linear/ADO HTTP clients, `PmAdapter` trait, identity resolver, ticket pattern matcher |
| `tga::classify` | `src/classify/` | Stage 2: classification cascade (override / issue_type / jira_mapping / LLM / rule-based), LLM client, taxonomy remap |
| `tga::report` | `src/report/` | Stage 3: CSV/JSON/Markdown report generation, DORA / velocity / quality / activity metrics |
| `commands` (binary-private) | `src/commands/` | Clap subcommand handlers wired into `src/main.rs` |

Module dependency order: `core` ← `collect`, `classify`, `report` ← `main.rs`.

## Key Dependency Choices

### Git: `git2` (libgit2)

**Chosen over** `gitoxide`.

- **Reason**: `git2` is mature, stable, and has wide production use. `gitoxide` (pure Rust)
  is promising but its diff and revwalk APIs are still evolving.
- **Trade-off**: requires libgit2 C dependency, but `git2` bundles it via `libgit2-sys` so
  builds are self-contained.

### Database: `rusqlite`

**Chosen over** `diesel`, `sea-orm`, `sqlx`.

- **Reason**: SQLite-only with no async requirement; rusqlite's synchronous API maps
  cleanly to per-thread connections owned by `rayon` workers. Async ORMs (sqlx, sea-orm)
  add complexity without benefit for embedded SQLite.
- **Approach**: Single connection per worker thread with explicit transactions for batch
  writes. No connection pool needed.

### Async runtime: `tokio`

**Chosen over** `async-std`, `smol`.

- **Reason**: Industry standard, best ecosystem support, mature `reqwest` integration.

### HTTP client: `reqwest`

**Chosen over** raw `hyper`.

- **Reason**: Higher-level API, built-in JSON, automatic decompression, easier auth setup.
- **TLS**: `rustls-tls` feature (no OpenSSL dependency).

### CLI: `clap` v4 derive

**Chosen over** `structopt` (now merged into clap), `argh`.

- **Reason**: Mature, derive macro provides ergonomic typed subcommand structure with
  minimal boilerplate; matches the subcommand layout of the Python predecessor.

### Config: `serde_yaml` + `serde`

**Chosen over** `yaml-rust`.

- **Reason**: First-class serde integration → typed config structs with `#[derive(Deserialize)]`.

### Date/time: `chrono`

**Chosen over** `time`.

- **Reason**: Better ISO week support (`IsoWeek`, `Datelike::iso_week`), more mature
  serde integration, matches Python's `isocalendar()` output format.

### Parallelism

- **CPU-bound**: `rayon` (data parallelism, `par_iter`, scoped thread pool)
- **I/O-bound**: `tokio` (async runtime for HTTP, file I/O)
- **Bridge**: `tokio::task::spawn_blocking` for CPU work inside async contexts;
  `rayon::scope` for short-lived parallelism inside sync code

### Pattern matching: `aho-corasick`

- Compiles all classification patterns into a single FSM
- O(message_length) lookup for *all* patterns simultaneously (vs O(n_patterns × message_length))
- Used for both ticket extraction and rule-based classification

### Fuzzy matching: `strsim`

- `jaro_winkler` for name similarity
- Fast (no allocations), purpose-built for identity resolution

## Error Handling Strategy

| Layer | Library | Pattern |
|-------|---------|---------|
| Library modules (`tga::core`, `tga::collect`, `tga::classify`, `tga::report`) | `thiserror` | Define `enum FooError` per module with `#[from]` conversions for upstream errors |
| Binary entry point (`src/main.rs`, `src/commands/`) | `anyhow` | Use `anyhow::Result<()>` for `main()` and command handlers; convert via `?` |
| Never | `panic!`, `unwrap()`, `expect()` | Forbidden in library code; allowed in tests only |

Example library error:

```rust
#[derive(Debug, thiserror::Error)]
pub enum CollectError {
    #[error("git operation failed: {0}")]
    Git(#[from] git2::Error),
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("config invalid: {0}")]
    Config(String),
}
```

## Database Pattern

### Connection Management

- One `rusqlite::Connection` per worker thread (no shared pool needed; SQLite handles
  intra-process concurrency via WAL)
- Per-connection setup:

```rust
conn.execute_batch("
    PRAGMA journal_mode = WAL;
    PRAGMA synchronous = NORMAL;
    PRAGMA foreign_keys = ON;
    PRAGMA mmap_size = 268435456;
")?;
```

### Query Wrappers

No raw SQL at call sites. Each table has a module in `tga::core::db` exposing typed
wrappers:

```rust
// src/core/db/cached_commits.rs
pub fn insert_commit(conn: &Connection, commit: &CommitRecord) -> Result<i64, DbError> { … }
pub fn list_by_iso_week(conn: &Connection, iso_week: &str) -> Result<Vec<CommitRecord>, DbError> { … }
```

### Migrations

- Migration runner applies versioned SQL migrations on startup
- Files in `src/core/db/sql/0001_initial_schema.sql` … `0011_pr_reviewers.sql`
- `schema_migrations` table tracks applied versions
- Future migrations added as `0012_*.sql`, etc.

## Configuration Deserialization

```rust
#[derive(Debug, Deserialize)]
pub struct Config {
    pub repositories: Vec<RepositoryConfig>,
    #[serde(default)]
    pub github: GitHubConfig,
    #[serde(default)]
    pub analysis: AnalysisConfig,
    // …
}

impl Config {
    pub fn from_path(path: &Path) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(shellexpand::tilde(path.to_str().unwrap()).as_ref())?;
        let mut cfg: Config = serde_yaml::from_str(&raw)?;
        cfg.expand_env_vars()?;
        cfg.validate()?;
        Ok(cfg)
    }
}
```

- `#[serde(default)]` on optional fields with sensible defaults via `Default` impls
- `~` expansion via `shellexpand`
- `${VAR}` env var expansion as a post-parse pass for secret fields (`token`, `api_key`)
- `validate()` performs cross-field consistency checks (e.g. `activity_scoring` weights sum to 1.0)

## Classification Dispatcher

```rust
pub enum ChangeType {
    Feature, Bugfix, Platform, Refactor, Documentation,
    Test, Maintenance, Style, Build, Security, Hotfix,
    Revert, Integration, Content, Localization, Research,
    Ktlo, Other, Unknown,
}

pub enum ClassificationTier {
    Override, IssueType, JiraMapping, Llm, RuleBased,
}

pub struct ClassificationResult {
    pub change_type: ChangeType,
    pub work_type: String,
    pub confidence: f32,
    pub tier: ClassificationTier,
}

#[async_trait]
pub trait Classifier {
    async fn classify(&self, commit: &CommitRecord) -> Option<ClassificationResult>;
}
```

Cascade dispatcher iterates classifiers in priority order, returning the first `Some(result)`:

```rust
for classifier in &self.classifiers {
    if let Some(result) = classifier.classify(commit).await {
        return result;
    }
}
self.rule_based.classify(commit).await.expect("rule-based always returns Some")
```

## Testing Strategy

| Layer | Approach |
|-------|----------|
| Unit tests | `#[cfg(test)] mod tests` inside each module |
| Integration tests | `tests/` directory per crate |
| HTTP mocking | `wiremock` crate (async test server) for GitHub/JIRA tests |
| Database tests | `rusqlite::Connection::open_in_memory()` |
| Property-based tests | `proptest` for parsers (ticket regex, ISO week parsing, version strings) |
| Snapshot tests | `insta` for CSV/JSON report output |

Target coverage: 90% on parsers and classifiers; 80% overall.

## Performance Targets (vs Python predecessor)

| Operation | Python | Rust Target | Factor |
|-----------|--------|-------------|--------|
| Repository extraction (commits/sec) | ~200 | 1500–2000 | 5–10x |
| Classification dispatch (commits/sec, rule-based) | ~5000 | 25000+ | 3–5x |
| API fetching (PRs/sec, with rate limiting) | ~5 | 15–20 | 2–4x |
| Overall pipeline (4-week, 10-repo run) | 3–5 min | 30–60 sec | 4–8x |

These are aspirational and will be validated with benchmark suites in `benches/tga_bench.rs`
using `criterion`.

## Architecture Decision Records

Significant design decisions are documented as ADRs in
`docs/trusty-git-analytics/decisions/`. See
`docs/trusty-git-analytics/decisions/README.md` for the format. New ADRs should be written for: library
choices, schema decisions, performance trade-offs, and any decision that
future contributors would otherwise have to reverse-engineer.

## Future Improvements

- **gitoxide migration**: once its diff API stabilizes, evaluate replacement for `git2`
  to eliminate libgit2 C dependency
- **In-process LLM**: optional feature flag for `candle` / `llama.cpp` Rust bindings to
  run small classification models without HTTP
- **Parquet export**: optional `parquet` crate output for analytics pipelines
- **Distributed cache**: optional Redis backend for multi-host pipelines
