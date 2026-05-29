# Developer Guide

This guide is for engineers who want to extend `tga`, integrate it with new data sources
or output formats, or understand the codebase well enough to contribute confidently.

---

## Table of Contents

1. [Architecture Overview](#1-architecture-overview)
2. [Module Responsibilities](#2-module-responsibilities)
3. [Classification Pipeline Deep-Dive](#3-classification-pipeline-deep-dive)
4. [How to Add a New PM Integration](#4-how-to-add-a-new-pm-integration)
5. [How to Add a New Output Format](#5-how-to-add-a-new-output-format)
6. [Database Migrations](#6-database-migrations)
7. [Coding Standards](#7-coding-standards)
8. [Testing](#8-testing)

---

## 1. Architecture Overview

`tga` is a single Rust crate that exposes both a library (`tga::*`) and a binary
(`tga`). All analytical logic lives in four library modules; the binary entry point
(`src/main.rs`) and its subcommand handlers (`src/commands/`) are binary-private.

### Three-stage pipeline

```
  git repos        external APIs
      │                  │
      ▼                  ▼
  ┌─────────────────────────────┐
  │  Stage 1: collect           │  tga::collect
  │  git2 revwalk + HTTP clients│
  └──────────────┬──────────────┘
                 │ SQLite (tga.db)
                 ▼
  ┌─────────────────────────────┐
  │  Stage 2: classify          │  tga::classify
  │  Four-tier cascade          │
  └──────────────┬──────────────┘
                 │ SQLite (tga.db)
                 ▼
  ┌─────────────────────────────┐
  │  Stage 3: report            │  tga::report
  │  CSV / JSON / Markdown      │
  └──────────────┬──────────────┘
                 │
                 ▼
              ./reports/
```

### Module dependency order

```
tga::core
    │
    ├──► tga::collect  ──────────────────────────────┐
    │                                                 │
    ├──► tga::classify ──────────────────────────────┤
    │                                                 │
    └──► tga::report   ──────────────────────────────►  src/main.rs (binary)
                                                         src/commands/ (binary-private)
```

`tga::core` has no internal dependencies. All other library modules depend only on
`tga::core`. The binary depends on all four library modules. `src/commands/` is
compiled only as part of the binary and is not part of the public API.

### File layout

```
src/
├── main.rs                     # Binary entry point (#[tokio::main])
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
├── core/                       # tga::core
│   ├── config/                 # serde_yaml config structs
│   ├── db/                     # rusqlite wrapper + migration runner
│   │   └── sql/                # Embedded SQL migration files
│   ├── errors.rs               # TgaError (thiserror)
│   └── models/                 # Domain structs (Commit, Author, etc.)
├── collect/                    # tga::collect
│   ├── git/                    # git2 revwalk + diff extraction
│   ├── github/                 # GitHub REST client (reqwest)
│   ├── bitbucket/              # Bitbucket REST client
│   ├── jira/                   # JIRA REST client
│   ├── linear/                 # Linear GraphQL client
│   ├── azdo/                   # Azure DevOps REST client
│   ├── identity/               # IdentityResolver
│   ├── pm_adapter.rs           # PmAdapter trait
│   ├── pr_provider.rs          # PrProvider trait
│   └── collector.rs            # CollectionPipeline
├── classify/                   # tga::classify
│   ├── tiers/                  # exact, regex, fuzzy, llm
│   ├── rules/                  # Rule types, loader, default rules
│   ├── pipeline.rs             # ClassificationPipeline
│   └── classifier.rs           # ClassificationEngine
└── report/                     # tga::report
    ├── formatters/             # csv, json, markdown writers
    ├── aggregator.rs           # Data aggregation queries
    ├── models.rs               # Report-level structs
    └── pipeline.rs             # ReportPipeline
```

---

## 2. Module Responsibilities

### tga::core

The foundation layer. No internal dependencies.

- **`config/`**: YAML deserialization via `serde_yaml`. The top-level struct is `Config`.
  All fields use `#[serde(default)]` so unknown keys and missing optional fields never
  cause parse errors. Path fields are `PathBuf`; callers apply `expand_path()` before I/O.
- **`db/`**: `Database` struct wraps a `rusqlite::Connection`. `Database::open()` applies
  WAL mode, enables foreign keys, and runs all pending migrations. Each migration executes
  inside a transaction.
- **`errors.rs`**: `TgaError` enum with `thiserror` derives. Used only within `tga::core`.
- **`models/`**: Domain structs shared across modules: `Commit`, `Author`,
  `Classification`, `PullRequest`, `WorkItem`, etc. All use `serde` derives for
  SQLite row mapping.

### tga::collect

Stage 1 pipeline.

- **`git/`**: Uses `git2` to walk the commit graph between a `since` and `until`
  boundary. Extracts commit metadata, diff stats, and author information. Does not
  fork the `git` subprocess.
- **`identity/`**: `IdentityResolver` maps raw `(author_name, author_email)` pairs to
  canonical identities using exact alias lookup followed by Jaro-Winkler fuzzy matching
  (`strsim`).
- **`github/`**, **`bitbucket/`**, **`jira/`**, **`linear/`**, **`azdo/`**: HTTP clients
  using `reqwest` + `tokio`. Each implements either `PrProvider` (for PR metadata) or
  `PmAdapter` (for ticket data), or both.
- **`collector.rs`**: `CollectionPipeline::run()` orchestrates per-repo git extraction,
  identity resolution, PR and ticket fetching, and upserts into `tga.db`.

### tga::classify

Stage 2 pipeline.

- **`rules/`**: `Rule` struct (keyword list, regex patterns, category, priority,
  confidence). `RuleLoader` reads a YAML rules file and merges with (or replaces) the
  80+ built-in default rules.
- **`tiers/`**: Four independent matchers — see
  [Classification Pipeline Deep-Dive](#3-classification-pipeline-deep-dive).
- **`classifier.rs`**: `ClassificationEngine` holds all four tiers. `classify_batch()`
  runs tiers 0–2 in parallel via `rayon`. `classify().await` is async and adds the LLM
  fallback (Tier 3).
- **`pipeline.rs`**: `ClassificationPipeline::run()` loads unclassified commits from
  `tga.db`, runs `classify_batch()` via Rayon, then calls the async `classify()` for any
  remaining unclassified commits when LLM is enabled.

### tga::report

Stage 3 pipeline.

- **`aggregator.rs`**: SQL queries that join `commits`, `classifications`, `authors`, and
  `pull_requests` into report-ready result sets.
- **`formatters/csv`**: Writes 9 CSV files using the `csv` crate.
- **`formatters/json`**: Writes 4 JSON files using `serde_json`.
- **`formatters/markdown`**: Renders `report.md` using `tera` templates (Jinja2-compatible).
- **`pipeline.rs`**: `ReportPipeline::run()` calls the aggregator then fans out to each
  registered formatter.

---

## 3. Classification Pipeline Deep-Dive

The cascade runs four tiers in order. The first tier to produce a result above
`confidence_threshold` wins. Commits not matched by any tier receive the label
`uncategorized` with confidence 0.0.

### Tier 0 — Overrides table

Before any rule is consulted, `ClassificationEngine` queries the `classification_overrides`
table for the commit SHA. If a row exists, its `work_type` and `change_type` are returned
with confidence 1.0 and method `manual`. No further tiers are evaluated.

Overrides are managed via `tga override add|list|remove`. They survive
re-classification — `tga classify` always respects Tier 0 entries.

### Tier 1 — Exact keyword matching (Aho-Corasick)

`ExactMatcher` feeds all `keywords` fields from all loaded rules into a single
Aho-Corasick automaton at construction time. Classification is one O(n) scan of the
commit message, where n is the message length regardless of the number of keywords.

Rules are sorted by `priority` (highest first) before automaton construction. When a
keyword fires, the associated rule's category, subcategory, and confidence are returned.

The `aho-corasick` crate implements the multi-pattern search.

### Tier 2 — Regex matching

`RegexMatcher` compiles each `pattern` field from each rule into a `regex::Regex` at
construction. Classification iterates rules in priority order and applies each compiled
regex. The first match returns the rule's classification.

`RegexMatcher::extract_ticket_id()` is a standalone helper that applies the configured
`ticket_regex` (or the default JIRA pattern `\b[A-Z][A-Z0-9]+-\d+\b`) to extract ticket
references. Both Tier 1 and Tier 2 call this helper to populate `ticket_id` on their
results.

### Tier 2.5 — Fuzzy / structural heuristics

`FuzzyClassifier` applies structural rules that require no pattern data:

- If the commit has `is_merge: true`: return category `merge`, confidence 0.8.
- If the message starts with `"Merge pull request"` or `"Merge branch"` (case-insensitive):
  return category `merge`, confidence 0.8.
- If the message starts with `"Revert "` (case-insensitive): return category `revert`,
  confidence 0.8.

All fuzzy results use `ClassificationMethod::FuzzyMatch`.

The `strsim` crate provides Jaro-Winkler similarity for identity resolution (not used
in commit classification tiers).

### Tier 3 — LLM fallback (async)

`LlmClassifier` is only invoked when `use_llm: true` is set in config and an API key
is available. It formats the commit message into a prompt requesting a JSON object
`{"category": "...", "confidence": 0.0}` and sends it to the configured provider
(OpenRouter, OpenAI, or Bedrock).

Responses below `confidence_threshold` are discarded. Malformed JSON causes the tier
to return `None` (not a hard error), and the commit falls through to `uncategorized`.

LLM requests are sent concurrently up to `llm_fallback_concurrency` (default: 8).

The LLM tier is async (`tokio`). It is never called from `classify_batch()`. The
`ClassificationPipeline` runs the Rayon batch first, collects commits still without a
result, then calls the async LLM path sequentially (from within the `tokio` runtime).

### Confidence threshold and progression

Each tier's result includes a `confidence` field. If a tier's confidence is below
`confidence_threshold`, the result is discarded and the next tier is tried. This means
a well-matched Tier 1 exact rule with confidence 0.95 does not trigger the LLM even when
`use_llm: true`.

The `llm_fallback_threshold` setting provides a coarser gate: any commit whose
best rule-based confidence exceeds this value skips the LLM entirely (even if below
`confidence_threshold`). Setting `llm_fallback_threshold: 0.5` prevents API calls for
commits that are "good enough."

### Backfill complexity

`tga classify --backfill-complexity` runs a second pass that queries commits with
a classification but no complexity score, then assigns a score from 1 (trivial) to 5
(high complexity) using a heuristic based on lines changed, file count, and the
classification tier used. It does not re-run the full classification cascade.

---

## 4. How to Add a New PM Integration

To add a new project management source (e.g. ClickUp, Shortcut):

### 4.1 Add the config struct

In `src/core/config/`, create a new struct (e.g. `ClickUpConfig`) with `serde::Deserialize`
and add it as an optional field to the top-level `Config` struct:

```rust
// src/core/config/clickup.rs
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ClickUpConfig {
    pub api_token: String,
    pub workspace_id: String,
    pub fetch_on_reference: bool,
    pub ticket_regex: Option<String>,
}
```

```rust
// src/core/config/mod.rs
pub struct Config {
    // ... existing fields ...
    pub clickup: Option<ClickUpConfig>,
}
```

### 4.2 Implement the traits

In `src/collect/`, create a new module (e.g. `src/collect/clickup/`). Implement
the relevant trait(s):

- **`PmAdapter`**: for fetching work items/tickets. Required method:
  `fetch_work_items(&self, refs: &[&str]) -> Result<Vec<WorkItem>>`.
- **`PrProvider`**: for fetching pull requests. Required method:
  `fetch_prs(&self, since: DateTime, until: DateTime) -> Result<Vec<PullRequest>>`.

Use `reqwest` + `tokio` for HTTP. Follow the pattern established in
`src/collect/azdo/` (which implements `PmAdapter`) or `src/collect/github/`
(which implements `PrProvider`).

### 4.3 Register in build_adapters()

In `src/collect/collector.rs`, add your new adapter to `build_adapters()`:

```rust
fn build_adapters(config: &Config) -> Vec<Box<dyn PmAdapter>> {
    let mut adapters: Vec<Box<dyn PmAdapter>> = vec![];
    // ... existing adapters ...
    if let Some(clickup_cfg) = &config.clickup {
        adapters.push(Box::new(ClickUpAdapter::new(clickup_cfg)));
    }
    adapters
}
```

### 4.4 Add a database migration if needed

If your integration stores additional data (e.g. a `clickup_issues` table), add a new
SQL migration file — see [Database Migrations](#6-database-migrations).

---

## 5. How to Add a New Output Format

To add a new report format (e.g. Excel, HTML):

### 5.1 Implement the formatter

Create a new file in `src/report/formatters/` (e.g. `excel.rs`). Implement the
`ReportFormatter` trait:

```rust
pub trait ReportFormatter {
    fn write(&self, data: &ReportData, output_dir: &Path) -> Result<()>;
    fn format_name(&self) -> &'static str;
}
```

`ReportData` (defined in `src/report/models.rs`) contains all pre-aggregated result
sets from the database.

### 5.2 Register in ReportPipeline

In `src/report/pipeline.rs`, add your formatter to the pipeline builder:

```rust
pub fn build_formatters(formats: &[String]) -> Vec<Box<dyn ReportFormatter>> {
    formats.iter().filter_map(|f| match f.as_str() {
        "csv"      => Some(Box::new(CsvFormatter::new()) as Box<dyn ReportFormatter>),
        "json"     => Some(Box::new(JsonFormatter::new()) as Box<dyn ReportFormatter>),
        "markdown" => Some(Box::new(MarkdownFormatter::new()) as Box<dyn ReportFormatter>),
        "excel"    => Some(Box::new(ExcelFormatter::new()) as Box<dyn ReportFormatter>),
        other      => { warn!("unknown format: {other}"); None }
    }).collect()
}
```

Users can then enable it via `output.formats: [csv, excel]` in config or
`tga report --formats excel`.

---

## 6. Database Migrations

`tga` uses a migration runner that applies numbered SQL files in order on every
`Database::open()`.

### Location

SQL files live in `src/core/db/sql/`. They are embedded at compile time via
`include_str!()` and referenced by the `MIGRATIONS` constant in
`src/core/db/migrations.rs`.

### Adding a migration

1. Create a new file: `src/core/db/sql/NNNN_description.sql` where `NNNN` is the
   next sequential integer.
2. Write idempotent SQL (safe to call on a database that already has the schema).
   Use `CREATE TABLE IF NOT EXISTS` and `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`.
3. Register the migration in `migrations.rs`:

```rust
Migration {
    version: NNNN,
    name: "description",
    sql: include_str!("sql/NNNN_description.sql"),
},
```

The migration runner applies any migrations with `version > current_version` inside a
transaction that also inserts the version record. Partial application is impossible.

### Never modify existing migrations

If a migration has been applied to a production database, changing it will cause a
version mismatch. Always add a new migration instead of editing an existing one.

### Current migration count

`tga` has 13 migrations (files `0001` through `0013`). Future migrations continue the
sequence from `0014`.

---

## 7. Coding Standards

These rules are enforced in CI. PRs that violate them will not be merged.

### Error handling

- Use `anyhow::Result` in `src/main.rs` and all files in `src/commands/`.
- Use `thiserror` error enums in all library modules (`src/core/`, `src/collect/`,
  `src/classify/`, `src/report/`).
- Do not mix the two: library modules must not import `anyhow`. The binary lets `?`
  convert library errors via `std::error::Error`.
- No `unwrap()` or `expect()` in library code. Propagate errors with `?`.
- `expect()` is acceptable in test code, with a descriptive message.

### Logging

Use `tracing::{info, warn, error, debug, trace}`. Never use `println!` or `eprintln!`
in library code or command handlers. Log levels:

- `error`: unrecoverable failures (after which the process exits or the operation is
  skipped with a hard warning).
- `warn`: degraded operation (e.g. fetch failed, continuing with local refs).
- `info`: significant milestones (pipeline start/end, commit counts, file writes).
- `debug`: per-item decisions (individual commit classification results).
- `trace`: raw HTTP request/response bodies.

### Concurrency

- Use `rayon::par_iter()` for CPU-bound batch operations (commit classification tiers
  0–2, diff stat aggregation).
- Use `tokio` for all async I/O (HTTP clients, LLM calls). Do not mix runtimes.
- `git2` repository handles are not `Send`. Open and drop them within a single
  per-repo processing block; never share across threads.

### Public API documentation

All `pub` items in library modules must have doc comments (`///`). Binary-private
items in `src/commands/` do not require doc comments.

### CI gates

All PRs must pass:

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo test --workspace
```

Run these locally before pushing:

```bash
cargo clippy -- -D warnings
cargo fmt
cargo test
```

---

## 8. Testing

### Running tests

```bash
cargo test
```

This runs all unit tests (located inline in modules using `#[cfg(test)]` blocks) and
doc tests.

### Integration test

One integration test is gated behind an environment variable to prevent it from running
in CI without a real repository:

```bash
INTEGRATION_REPO_PATH=/path/to/some/git/repo cargo test --test integration
```

The integration test runs a full collect → classify → report pipeline against the
specified repository and asserts that output files are produced with non-zero content.

### HTTP mocking

Tests that exercise HTTP clients use `wiremock` (listed in `[dev-dependencies]`) to
run a mock HTTP server. Tests set up stub responses and assert that the correct requests
were made. No real network calls are made in the test suite.

```rust
#[tokio::test]
async fn github_fetch_prs_handles_pagination() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/api/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&pr_page_1()))
        .mount(&mock_server)
        .await;
    // ...
}
```

### Adding tests

- Unit tests live in the same file as the code they test, in a `#[cfg(test)]` module.
- Integration tests live in `tests/`.
- Property-based tests may use the `proptest` crate (add to `[dev-dependencies]` if needed).
- New features require accompanying tests. The PR template will ask you to confirm
  coverage.
