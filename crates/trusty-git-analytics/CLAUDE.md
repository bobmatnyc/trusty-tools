# trusty-git-analytics — AI Assistant Instructions

## Project Purpose

This is a **Rust port** of `gitflow-analytics` — a developer productivity analytics tool.
- Python predecessor: `/Users/masa/Projects/gitflow-analytics`
- Python predecessor GitHub: https://github.com/bobmatnyc/gitflow-analytics

The goal is full API compatibility with the Python tool, using Rust best practices for
superior performance, parallelism, and correctness.

## Implementation State

> Last updated: 2026-05-12

| Component | Status | Notes |
|-----------|--------|-------|
| Single `tga` crate | DONE | Consolidated from 5-crate workspace into one library+binary |
| `src/core/` | DONE | Types, config, DB schema, error definitions |
| `src/collect/` | DONE | git2 extraction, identity resolution, GitHub/Bitbucket/JIRA/Linear/ADO clients, `PmAdapter` + `PrProvider` traits |
| `src/classify/` | DONE | Four-tier classification cascade (rules + LLM) |
| `src/report/` | DONE | CSV/JSON/Markdown report generation |
| `src/main.rs` + `src/commands/` | DONE | clap CLI binary entry point |
| Database migrations | DONE | v1 initial schema in `src/core/db/sql/` |
| Configuration structs | DONE | YAML schema implemented in `src/core/config/` |
| Tests | DONE | 37 unit tests + 1 gated integration test |
| CI/CD | DONE | GitHub Actions workflows for build, test, publish |

## Architecture Overview

Three-stage pipeline implemented as a single crate. The binary `tga` is
built from `src/main.rs`; the library (`tga::*`) is reusable from
integration tests, external code, and the binary itself.

| Module | Path | Purpose |
|--------|------|---------|
| `tga::core` | `src/core/` | Shared types, config (serde), DB schema (rusqlite), error types |
| `tga::collect` | `src/collect/` | Stage 1: git extraction (git2), GitHub/JIRA HTTP clients (reqwest+tokio) |
| `tga::classify` | `src/classify/` | Stage 2: four-tier classification cascade (rules + LLM) |
| `tga::report` | `src/report/` | Stage 3: CSV/JSON/Markdown generation |
| `commands` (bin-private) | `src/commands/` | Subcommand handlers wired into `main.rs` |

### Module Dependency Order

```
core  <──  collect   <──┐
      <──  classify  <──┤  main.rs (binary)
      <──  report    <──┘
```

## Key Rust Decisions

- **git2**: libgit2 bindings for git operations (replaces GitPython + subprocess)
- **rusqlite**: SQLite with `bundled` feature — no system SQLite required
- **tokio**: async runtime for all HTTP clients
- **rayon**: data parallelism for batch commit processing
- **clap**: CLI with derive macros (same subcommand structure as Python)
- **serde + serde_yaml**: config deserialization (same YAML schema as Python)
- **aho-corasick**: multi-pattern commit message matching
- **strsim**: fuzzy string matching for identity resolution
- **chrono**: date/time with ISO week support
- **tera**: Jinja2-style templates for Markdown reports
- **blake3**: config file hashing
- **anyhow + thiserror**: error handling (anyhow in bins, thiserror in libs)

## Database

SQLite, same schema as `gitflow-analytics`. Schema defined in `src/core/db/`.
Migration runner applies versioned SQL migrations on startup (v1–v18 from Python port, +future).

🔴 **Critical**: Always use WAL journal mode: `PRAGMA journal_mode=WAL`.

Reference: `docs/trusty-git-analytics/requirements/database-schema.md`

## Configuration

YAML file, same structure as Python version. Deserialized via `serde_yaml` into structs in
`src/core/config/`. Support `~` expansion for paths.

Reference: `docs/trusty-git-analytics/requirements/configuration.md`

## CLI Structure

Binary: `tga` (produced by `src/main.rs`)

Subcommands: `analyze` (full pipeline), `collect` (stage 1), `classify` (stage 2),
             `report` (stage 3)

Reference: `docs/trusty-git-analytics/requirements/cli-commands.md`

## Development Commands

The ONE canonical way to perform each task:

```bash
# Build everything
cargo build

# Build release binary
cargo build --release          # output: target/release/tga

# Run all tests
cargo test

# Lint (must pass with zero warnings)
cargo clippy -- -D warnings

# Format check (CI gate)
cargo fmt --check

# Format (auto-fix)
cargo fmt

# Generate and open API docs
cargo doc --open

# Run the CLI (dev)
cargo run --bin tga -- <subcommand>

# Check the crate
cargo check
```

🔴 **CI requirements**: `cargo clippy -- -D warnings` and `cargo fmt --check` must both pass before merging.

## Priority Rankings

### 🔴 Critical
- `core`: error types, config structs, DB schema, migration runner
- WAL mode pragma on every DB open
- `anyhow` in `main.rs`, `thiserror` enums in library modules (never mix)

### 🟡 Important
- `collect`: git2 commit extraction, identity resolution, GitHub/JIRA clients
- `classify`: four-tier cascade (exact rules → regex → fuzzy → LLM fallback)
- `commands`: clap subcommand wiring

### 🟢 Nice-to-have
- `report`: CSV/JSON output first, Markdown templates later
- Progress bars (indicatif) in long-running operations
- `--dry-run` flags on mutating commands

### ⚪ Informational
- `docs/trusty-git-analytics/requirements/` contains full specification — read before implementing any module
- Python predecessor at `/Users/masa/Projects/gitflow-analytics` for reference behavior
- KuzuMemory MCP tools (`kuzu_recall`, `kuzu_learn`, `kuzu_enhance`) available for context

## Requirements Reference

All specification documents are in `docs/trusty-git-analytics/requirements/`:

| File | Covers |
|------|--------|
| `overview.md` | System overview and pipeline |
| `configuration.md` | Full YAML config schema |
| `database-schema.md` | All SQLite tables and columns |
| `cli-commands.md` | All subcommands and flags |
| `classification.md` | Four-tier classification cascade |
| `collection.md` | Git extraction and API fetching |
| `reporting.md` | Report formats and metrics |
| `rust-architecture.md` | Rust-specific design decisions |
| `index.md` | Requirements index |

## Coding Standards

- Use `anyhow::Result` in the binary (`src/main.rs` and `src/commands/`), `thiserror` enums in library modules (`core`, `collect`, `classify`, `report`)
- Prefer `tracing::{info, warn, error, debug}` over `println!` / `eprintln!`
- All public API items must have doc comments (`///`)
- No `unwrap()` or `expect()` in library code — propagate errors with `?`
- Use `rayon::par_iter()` for CPU-bound batch operations (commit classification)
- All async functions use `tokio` — no mixing of async runtimes

## Claude MPM Configuration

See `.claude-mpm/` for claude-mpm project configuration.
