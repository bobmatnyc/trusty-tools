# Contributing

## Development setup

### Prerequisites

- Rust stable 1.80+ (`rustup show`)
- Git
- A `.env.local` with at minimum `OPENROUTER_API_KEY` for live integration tests

### First-time setup

```bash
git clone https://github.com/bobmatnyc/open-mpm
cd open-mpm
cat > .env.local <<'EOF'
OPENROUTER_API_KEY=sk-or-v1-...
# ANTHROPIC_API_KEY=sk-ant-api03-...
# BRAVE_API_KEY=BSA...
EOF

cargo build
cargo test
```

All unit tests pass without API keys. Integration tests that require a live
API key are gated on the env var being set.

## Project layout

```
open-mpm/
├── Cargo.toml             Rust package manifest
├── build.rs               Captures GIT_COMMIT_HASH at compile time
├── CLAUDE.md              Architectural reference for AI-assisted development
├── Makefile               Convenience wrappers around cargo commands
├── .env.local             API keys (not committed)
├── .open-mpm/             Bundled config + runtime state (state/ is gitignored)
├── docs/
│   ├── user/              User-facing docs (quickstart, CLI, configuration)
│   ├── developer/         Developer docs (architecture, contributing, building, testing)
│   ├── design/            Design goals + system design + ADRs
│   ├── research/          Research notes (preserved as-is)
│   ├── performance/       Per-run telemetry JSON (auto-generated)
│   └── _archive/          Outdated or superseded docs
├── src/                   Rust source (see developer/architecture.md)
├── tests/                 Integration tests
└── ui/                    Vite-built web UI (embedded into binary)
```

## Coding standards

### Rust

- **Edition**: 2024 (set in `Cargo.toml`)
- **Formatting**: `cargo fmt` — CI fails on diff
- **Linting**: `cargo clippy --all-targets` — no new warnings
- **Tests**: every public function should have at least one unit test.
  Async tests use `#[tokio::test]`.
- **Error handling**: `anyhow::Result<T>` for application code,
  `thiserror` for library-style error enums (`WorkflowError`, etc.)
- **Logging**: `tracing` macros (`info!`, `warn!`, `error!`, `debug!`) —
  no `println!` in library paths; `eprintln!` is acceptable for
  user-facing CLI output

### Documentation

Every function, method, and module should carry a docstring with three
parts (Why / What / Test):

```rust
/// Why: …intent and motivation…
/// What: …one-line behavioral summary…
/// Test: …how to verify…
pub fn foo(x: u32) -> u32 { x + 1 }
```

See the existing codebase for examples — most modules in `src/api`,
`src/ctrl`, and `src/workflow` already follow this convention.

### Commit messages

Conventional commits with optional ticket reference:

```
feat: add CTRL search_docs tool with TF-IDF index (#187)

…body…
```

Common prefixes: `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `chore`.

### File size

Soft limit ~800 lines per file. Flag for refactor at 600. `src/main.rs`,
`src/ctrl/mod.rs`, and `src/api/server.rs` already exceed this and are
candidates for splitting in future PRs.

## Workflow for a change

1. Create a branch from `main`
2. Make atomic commits — each one should compile and pass tests
3. Run the full check suite locally:

```bash
cargo fmt
cargo clippy --all-targets
cargo test --bins
```

4. Open a PR; reference the GitHub issue if one exists
5. Wait for CI to pass

## Bug reports and feature requests

File issues on GitHub. Include:

- Version: `open-mpm --version`
- OS/arch: macOS-arm64, Linux-x64, etc.
- Reproduction: minimal `cargo run --` invocation
- Logs: re-run with `RUST_LOG=debug`

## Useful Make targets

```bash
make build      # cargo build
make test       # cargo test
make clippy     # cargo clippy --all-targets -- -D warnings
make fmt        # cargo fmt
make lint       # clippy + fmt
make ctrl       # cargo run -- --ctrl
make release    # cargo build --release
make clean      # cargo clean
make version    # print semver from Cargo.toml
```
