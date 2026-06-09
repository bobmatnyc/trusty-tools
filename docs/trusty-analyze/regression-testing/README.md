# Regression Testing & Benchmark Snapshots — trusty-analyze

This folder contains snapshots of trusty-analyze performance benchmarks, one per
release that is measured. Issue #948 established the first baseline (v0.5.1).

## Purpose

Snapshots provide:
- **Baseline data** for each release: per-tool latency percentiles, error rates
- **Corpus quality metrics**: avg_cyclomatic, pct_grade_a, smell_count per release
- **Historical context**: Performance trajectory across releases
- **Regression detection**: Spot anomalies when new releases ship

## Methodology

**Canonical benchmark suite**: `crates/trusty-analyze/tests/stdio_harness.rs`

The `full_stdio_suite` test (gated behind `#[ignore]`):
1. Spawns a `target/release/trusty-analyze` child process in MCP stdio mode
2. Exercises all 17 tools via JSON-RPC over stdin/stdout
3. Reports latency distribution (p50/p95/p99/mean) for N=50 iterations per tool

**Run command**:
```bash
# Build release binary first
cargo build --release -p trusty-analyze

# Run the suite
cargo test -p trusty-analyze --test stdio_harness -- full_stdio_suite --ignored --nocapture
```

**Prerequisites**:
- trusty-search daemon running on port 7878 with a corpus indexed
- `trusty-tools` index present (or another index; suite auto-selects)

**Test corpus**: `trusty-tools` index (28,119 chunks as of v0.5.1 baseline).
Latency scales approximately linearly with corpus size for chunk-fetch-dominated
tools (`complexity_hotspots`, `find_smells`, `analyze_quality`).

## File Naming Convention

```
v{VERSION}-{YYYY-MM-DD}.md
```

## Snapshot Index

- [`v0.5.1-2026-06-09.md`](v0.5.1-2026-06-09.md) — First baseline. Full
  `full_stdio_suite` passed (413.57s). All 17 tools present and functional.
  `complexity_hotspots` p50=667ms, `analyze_quality` p50=1118ms,
  `list_entities` p50=3009ms. 0 errors across 350 calls.

## Key Metrics to Track

| Tool | v0.5.1 p50 | Alert if > |
|------|-----------|------------|
| `analyzer_health` | 0.42 ms | 10 ms |
| `complexity_hotspots` | 667 ms | 2,000 ms |
| `find_smells` | 701 ms | 2,000 ms |
| `analyze_quality` | 1,118 ms | 5,000 ms |
| `list_entities` | 3,009 ms | 10,000 ms |
| `suggest_refactors` | 2,330 ms | 5,000 ms |
| `list_facts` | 0.14 ms | 10 ms |

## Adding a New Snapshot

1. Build the release binary: `cargo build --release -p trusty-analyze`
2. Run the suite against a live trusty-search daemon with a known corpus.
3. Record results in `v{VERSION}-{YYYY-MM-DD}.md`.
4. Update the `current.md` symlink.

**Tracking issue**: create a `trusty-analyze` perf tracker analogous to
trusty-search's [#129](https://github.com/bobmatnyc/trusty-tools/issues/129).
