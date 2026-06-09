# Regression Testing & Benchmark Snapshots — trusty-memory

This folder contains snapshots of trusty-memory performance benchmarks, one per
release that is measured. Issue #948 established the first baseline (v0.15.0).

## Purpose

Snapshots provide:
- **Baseline data** for each release: latency percentiles, throughput, RSS
- **Historical context**: Performance trajectory across releases
- **Regression detection**: Spot anomalies when new releases ship

## Methodology

**Canonical benchmark suite**: `crates/trusty-memory/tests/concurrent_perf.rs`

Six `#[ignore]`-tagged integration tests:
- `test_http_concurrent_reads` — 50 tasks × 20 ops (reads)
- `test_http_concurrent_rw` — 20 writers + 20 readers × 10 ops each
- `test_http_burst` — 500 simultaneous requests via `join_all`
- `test_uds_concurrent` — 20 UDS connections × 10 pipelined requests
- `test_bridge_concurrent` — 10 concurrent MCP bridge processes
- `test_http_sustained_load` — 10 clients × 10 seconds continuous

**Run command**:
```bash
cargo test -p trusty-memory --test concurrent_perf -- --include-ignored --nocapture
```

**Prerequisite**: The daemon must be started from a directory with a project
marker (`.git`, `Cargo.toml`, etc.) — the `palace_create` call in the suite
requires a project context. The production launchd daemon (CWD `/`) cannot
serve this suite as of v0.15.0 without a `personal` palace workaround.

**Fallback**: Direct HTTP measurements via Python urllib + threading (used in
the v0.15.0 baseline) provide a reproducible alternative until the suite is
updated to use `personal` palaces.

## File Naming Convention

```
v{VERSION}-{YYYY-MM-DD}.md
```

## Snapshot Index

- [`v0.15.0-2026-06-09.md`](v0.15.0-2026-06-09.md) — First baseline. Direct
  HTTP measurement (canonical suite blocked by project-root constraint).
  `memory_recall` p50=0.3ms, 20-concurrent: 250 req/s, 0 errors.

## Adding a New Snapshot

1. Run the concurrent_perf suite (or direct HTTP measurements if suite blocked).
2. Record results in `v{VERSION}-{YYYY-MM-DD}.md`.
3. Update the `current.md` symlink.

**Tracking issue**: create a `trusty-memory` perf tracker analogous to
trusty-search's [#129](https://github.com/bobmatnyc/trusty-tools/issues/129).
