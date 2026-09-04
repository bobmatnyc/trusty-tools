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
Name what you changed — `--features memory-core,embedder-test-support` — or
`--features unconditional-only` for the always-compiled surface.
`--all-features` is unavailable: the `embedder-*` ORT variants are mutually
exclusive. `cargo build` and `cargo check -p trusty-common` are unaffected.

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
# (requires the daemon running and trusty-tools indexed)
cargo test -p trusty-search --test baseline_trusty_tools -- --include-ignored --nocapture
```

## Ignore-Tagged Integration Tests

ONNX-backed embedder tests are marked `#[ignore]` so CI stays fast. They do not
run under a plain `cargo test`. Use `--include-ignored` when you need local
validation against the actual model — and expect it to be slow.

## `trusty-mpm-gui` and `trusty-code-gui` Are Excluded by Default

Both are omitted from the root `Cargo.toml`'s `default-members` (#2951), so a
bare `cargo build` / `test` / `check` skips them. Name them explicitly
(`cargo build -p trusty-mpm-gui`) or use `--workspace`, which always builds
everything regardless of `default-members`.

## Crate Names vs. Directory Names

**Crate names** match the `name` field in each crate's `Cargo.toml`, which is
not always the directory name:

- `crates/trusty-git-analytics/` → `-p tga` (short published name)
- `crates/trusty-agents/` → `-p trusty-agents`

If you get "package not found", read the `name` field in that crate's
`Cargo.toml`. The abbreviation table in `CLAUDE.md` resolves the conversational
short names (`tm`, `ts`, `tc`, …) — expand those *before* composing a `-p` flag.

## Related

- `cargo-publish` — version bumps, tagging, and publishing to crates.io.
- `docs/reference/test-ladder-baseline.md` — per-rung gate command chains and
  the baseline-failure protocol.
