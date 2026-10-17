---
name: cargo-commands
description: Full cargo command reference for the trusty-tools workspace — release builds, feature-gated tests, ignored/ONNX integration tests, single-test-by-name runs, the trusty-search performance regression suite, dependency audit, and crate-name-vs-directory resolution
user-invocable: true
version: "1.0.0"
category: local-ops
tags: [cargo, build, test, workspace, local-ops]
effort: low
when_to_use: When you need a cargo invocation beyond the five everyday one-liners in CLAUDE.md — a release build, a feature-gated test, an ignored/ONNX integration test, a single test by name, the trusty-search baseline suite, cargo update/audit, or running a crate binary
---

# Cargo Commands — trusty-tools Workspace

🔴 **Single-path workflows — use exactly these commands.**

The five everyday one-liners (`cargo build`, `cargo check -p <crate>`,
`cargo test -p <crate> --no-fail-fast`,
`cargo clippy -p <crate> --all-targets -- -D warnings`,
`cargo fmt`) are resident in the root `CLAUDE.md` and are not repeated here.
This skill is the exhaustive variant list.

**Which gate to run for a given change is not this skill's call** — that is the
Rust Test Ladder in `CLAUDE.md`, with the per-rung command chains in
[docs/reference/test-ladder-baseline.md](../../../docs/reference/test-ladder-baseline.md).
This skill tells you how to *spell* a command, not whether you owe it.

## Two Rules That Apply to Every `cargo test` Here

🔴 **`--no-fail-fast` is not optional.** Cargo runs each test target as its own
binary and stops issuing further targets the moment one target reports a
failure — it does not run them all and report the aggregate. One failing `--lib`
test therefore hides every integration target behind it, and the run exits
having covered far less than its counts suggest. Name the flag beside any counts
you report. (See `CLAUDE.md`; this has been missed twice — issue #5324 and
PR #5904.)

🔴 **`trusty-common` takes `--features` on every test run.** Its default feature
set is empty, so a bare `cargo test -p trusty-common` is a `compile_error!`.
Name what you changed — `--features <feature>`, e.g. `--features embedder` — or
`--features unconditional-only` for the always-compiled surface.
`--all-features` is unavailable: the `embedder-*` ORT variants are mutually
exclusive. `cargo build` and `cargo check -p trusty-common` are unaffected.
The whole crate, every test once: `./scripts/test_trusty_common_lanes.sh`.
The memory engine left trusty-common in #9271 (its `memory-core` feature is a
no-op): test it with `cargo test -p trusty-memory --no-fail-fast`.

## Workspace-wide

```bash
# Build all crates (release/optimised)
cargo build --release

# Run all tests across the workspace
cargo test --no-fail-fast

# Lint workspace-wide, all targets
cargo clippy --workspace --all-targets -- -D warnings

# Format check (no writes) / format and fix
cargo fmt --check
cargo fmt

# Run ONNX-backed integration tests (slow; skipped in CI)
cargo test --no-fail-fast -- --include-ignored

# Update dependencies (review the Cargo.lock diff before committing)
cargo update

# Audit dependencies for known vulnerabilities
cargo audit   # requires: cargo install cargo-audit
```

## Individual Crate

```bash
# Build a single crate (dev / release)
cargo build -p trusty-search
cargo build --release -p trusty-search

# Build only one binary rather than the whole workspace
cargo build --release -p trusty-mpm

# Test a single crate with a specific feature
cargo test -p trusty-common --features axum-server --no-fail-fast

# Test a single test by name within a crate
cargo test -p trusty-search -- my_test_name

# Exact single test, uncaptured output (the flake-repro form)
cargo test -p <crate> <test> -- --exact --nocapture

# Test a single crate including its #[ignore]-tagged tests
cargo test -p trusty-embedderd --no-fail-fast -- --include-ignored

# Run a binary from a specific crate
cargo run -p trusty-search -- start
cargo run -p trusty-mpm -- --help

# trusty-search performance regression suite
# requires a sandbox daemon (scripts/sandbox_search_daemon.sh) with a marked
# corpus copy indexed; talks over its socket (#9993), no default socket.
# Optional TRUSTY_SEARCH_BENCH_ENVELOPE=<absolute path> writes the schema-1
# perf envelope (p50/p95/p99, n, qps per operation); unset writes nothing.
TRUSTY_SEARCH_BENCH_SOCKET=<sandbox dir>/data/trusty-search.sock \
TRUSTY_SEARCH_BENCH_INDEX=<index id> \
TRUSTY_SEARCH_TEST_CORPUS_ROOT=<marked corpus copy> \
  cargo test -p trusty-search --test baseline_trusty_tools -- --include-ignored --nocapture --test-threads=1

# trusty-search read-only storage probe (#9993 G6): per-file bytes, total,
# bytes/chunk and daemon RSS for the same sandbox index, as envelope rows.
# Same three variables; also needs the sandbox script's sandbox.pid.
  cargo test -p trusty-search --test storage_probe -- --include-ignored --nocapture
```

## Ignore-Tagged Integration Tests

ONNX-backed embedder tests are marked `#[ignore]` so CI stays fast. They do not
run under a plain `cargo test`. Use `--include-ignored` when you need local
validation against the actual model — and expect it to be slow.

## `trusty-code-gui` Is Excluded by Default

It is omitted from the root `Cargo.toml`'s `default-members`, so a bare
`cargo build` / `test` / `check` skips it. Name it explicitly
(`cargo build -p trusty-code-gui`) or use `--workspace`, which always builds
everything regardless of `default-members`.

## Crate Names vs. Directory Names

**Crate names** match the `name` field in each crate's `Cargo.toml`, which is
not always the directory name:

- `crates/trusty-agents/` → `-p trusty-agents`

If you get "package not found", read the `name` field in that crate's
`Cargo.toml`. The abbreviation table in `CLAUDE.md` resolves the conversational
short names (`tm`, `ts`, `tc`, …) — expand those *before* composing a `-p` flag.

## Related

- `cargo-publish` — version bumps, tagging, and publishing to crates.io.
- `docs/reference/test-ladder-baseline.md` — per-rung gate command chains and
  the baseline-failure protocol.
