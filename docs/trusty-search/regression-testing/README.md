# Regression Testing & Benchmark Snapshots

This folder contains snapshots of trusty-search performance benchmarks, one per release that is measured. The authoritative tracking issue is GitHub #129: [tracker(trusty-search): benchmark results across releases — regression testing](https://github.com/bobmatnyc/trusty-tools/issues/129).

## Purpose

Snapshots in this folder provide:
- **Baseline data** for each release: Hit@1, Hit@5, MRR, latency, memory
- **Historical context**: Performance trajectory across releases
- **Regression detection**: Spot anomalies when new releases ship
- **Investigation artifacts**: Raw results for deep-dive analysis when regressions are detected

## Methodology

All benchmarks use the same canonical setup:

**Benchmark suite**: 14-query suite in `crates/trusty-search/tests/baseline_trusty_tools.rs`

**Corpus**: trusty-tools repository itself (currently ~17k–21k chunks)

**Comparators**:
- `grep -rn` — baseline for recall (comprehensive but slow)
- `rg` (ripgrep) — mid-ground (fast, good recall)
- `trusty-search` — the system under test (speed + quality)

**Metrics**:
- **Hit@1**: Percentage of queries where expected result is rank 1
- **Hit@5**: Percentage of queries where expected result is in top 5
- **MRR**: Mean Reciprocal Rank (average 1/rank of expected results)
- **Latency**: Trusty-search processing time (server-side + client RTT)
  - Mean, median (p50), p95
- **Memory**: Peak RSS during indexing
- **Status deltas**: Intent transitions, field mutations (when relevant)

## File Naming Convention

```
v{VERSION}-{YYYY-MM-DD}.md
```

Examples:
- `v0.8.1-2026-05-25.md` — v0.8.1 benchmarked on May 25, 2026
- `v0.9.0-2026-06-15.md` — v0.9.0 benchmarked on June 15, 2026

## Snapshot Index

### Version snapshots (release-over-release)

- [`v0.10.0-2026-05-25.md`](v0.10.0-2026-05-25.md) — #138 per-lane MCP tools landed. Hit@1 67%, Hit@5 72-78% on open-mpm.
- [`v0.9.2-2026-05-25.md`](v0.9.2-2026-05-25.md) — #122 Function/Method Definition boost (metrics flat; blocked on #142+#143).
- [`v0.9.1-2026-05-25.md`](v0.9.1-2026-05-25.md) — Async spawn Phase 2 experiments (latency p95 flat).
- [`v0.9.0-2026-05-25.md`](v0.9.0-2026-05-25.md) — Staged pipeline Phase 1 shipped. Stage 1 (lexical-only, 302ms/100 files).
- [`v0.8.3-2026-05-25.md`](v0.8.3-2026-05-25.md) — Docs-by-default + QueryClassifier acronym fixes.
- [`v0.8.1-2026-05-25.md`](v0.8.1-2026-05-25.md) — Walker .gitignore fix (#100). First honest measurements post-corruption.

### Alternate-corpus baselines

- [`synthetic-corpus-baseline-2026-05-25.md`](synthetic-corpus-baseline-2026-05-25.md) — Non-circular 47-file synthetic corpus (#123 v2). Eliminates BM25 circular-bias contamination. Clean Hit@1: 43% lexical, 43% hybrid on definitions.
- [`open-mpm-baseline-2026-05-25.md`](open-mpm-baseline-2026-05-25.md) — First organic-corpus measurement (282 files / 6,611 chunks via v0.10.0).
- [`baseline-performance-2026-05-22.md`](baseline-performance-2026-05-22.md) — Pre-session baseline performance reference.
- [`search-perf-2026-05-12.md`](search-perf-2026-05-12.md) — Early search-performance measurements (migrated from the in-crate `docs/performance/`).

### Latest

[`current.md`](current.md) — symlink to the latest version snapshot (currently v0.13.1-2026-05-27).

## Caveat: BM25 Circular Bias (#123)

**IMPORTANT**: All snapshots recorded before GitHub issue #123 lands are contaminated by a **BM25 circular bias**:
- The test corpus is the trusty-tools repository itself
- The 14-query suite is checked into the repository
- BM25 indexes contain the queries as literal strings
- This artificially inflates Hit@1 and Hit@5 across all queries

**Mitigation**: Every pre-#123 snapshot includes a `## Caveat: BM25 Circular Bias` section and a warning emoji in the tracking issue table.

Once #123 is resolved (queries isolated, corpus cleaned), this caveat will be removed and new snapshots will provide uncontaminated baselines.

## Adding a New Snapshot

When benchmarking a release:

1. **Run the canonical suite**:
   ```bash
   cargo test -p trusty-search --test baseline_trusty_tools -- --include-ignored --nocapture
   ```

2. **Record results** in a new file following the naming convention:
   ```
   docs/regression-testing/v{VERSION}-{YYYY-MM-DD}.md
   ```

3. **Append a comment** to the tracking issue (#129) with:
   - One-line summary (e.g., "v0.9.0: Hit@1 +2.1%, Hit@5 +0.8%, latency -15%")
   - Link to the snapshot: `docs/regression-testing/v{VERSION}-{YYYY-MM-DD}.md`

4. **File a regression bug** if Hit@1 or Hit@5 regresses ≥ 5 percentage points vs. the prior release

## Snapshot Structure

Each snapshot `.md` file should include:

- **Header**: Version, date, corpus state (chunk count, disk size, composition)
- **Summary table**: 14 queries × (Hit rank, TS latency, rg latency, grep latency)
- **Aggregate metrics**: Hit@1, Hit@5, MRR, latency stats
- **Anomalies**: Any unexpected per-query results or performance patterns
- **Relevant PRs/fixes**: Links to issues/PRs that landed in this release
- **Caveat section** (if pre-#123): Callout on BM25 circular bias
- **Cross-links**: Link to tracking issue and related tickets

See `v0.8.1-2026-05-25.md` and `v0.8.3-2026-05-25.md` for template examples.

## Tracking & Detection

The tracking issue (#129) maintains an overview table. When a release regresses:

- **Minor regression** (<5 pp): Note in the snapshot; no new ticket unless it correlates with a known issue
- **Major regression** (≥5 pp): File a new `bug` ticket and link it from the comment on #129

Over time, this accumulation helps detect patterns:
- Features that consistently improve/degrade quality
- Trade-offs (e.g., latency vs. recall)
- Regressions that need immediate investigation

---

**Tracking issue**: [#129](https://github.com/bobmatnyc/trusty-tools/issues/129)
