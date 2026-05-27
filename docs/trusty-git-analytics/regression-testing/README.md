# tga Regression Testing & Benchmark Snapshots

This folder contains throughput and quality snapshots for `tga` (trusty-git-analytics)
releases, measured against real-world corpora.

## Purpose

Snapshots provide:
- **Baseline data** per release: throughput, coverage, method distribution, JIRA efficiency
- **Historical context**: Performance and coverage trajectory across versions
- **Regression detection**: Spot anomalies when new releases ship
- **Investigation artifacts**: Raw results for deep-dive analysis

## Methodology

All benchmarks use the same canonical setup when possible:

**Corpus**: Duetto `~/Duetto/cto` — 72,608-commit production corpus (20 JIRA projects).
This is a real-world, mixed-origin commit history. See each snapshot for corpus-specific caveats.

**Timing**: `/usr/bin/time -l` (macOS) reporting wall time, user+sys CPU time, and peak RSS.

**Runs**:
- **B1 (full)**: `tga classify --force --rules <rules>` — all tiers including JIRA external source
- **B5 (no-external)**: `tga classify --force --rules <rules> --no-external` — rule cascade only

**Metrics**:
- **Coverage**: percentage of corpus commits classified (vs. left uncategorized)
- **CPU throughput**: commits/sec at rule-cascade speed (B5, no JIRA latency)
- **Wall throughput**: commits/sec including JIRA API overhead (B1)
- **Method distribution**: per-tier commit counts (`regex_rule`, `weighted_sum`, `external_source`, `fuzzy_match`)
- **Peak RSS**: process memory at classify peak

## File Naming Convention

```
v{VERSION}-{YYYY-MM-DD}.md
```

Examples:
- `v1.3.0-2026-05-27.md` — v1.3.0 benchmarked on May 27, 2026

## Snapshot Index

### Version snapshots (release-over-release)

- [`v1.3.0-2026-05-27.md`](v1.3.0-2026-05-27.md) — **First published tga benchmark.** 72k-commit Duetto corpus. CPU throughput: ~113k commits/s (no JIRA). Coverage: 67.7% with JIRA, 64.3% without. Peak RSS: 235 MB. weighted_sum tier (new in 1.3.0) contributes 9.6–11.0% of classified commits.

### Latest

[`current.md`](current.md) — symlink to the latest version snapshot (currently v1.3.0-2026-05-27).

## Adding a New Snapshot

When benchmarking a release:

1. **Run the canonical suite**:
   ```bash
   cd ~/Duetto/cto
   set -a; source .env.local; set +a
   /usr/bin/time -l tga classify --force --rules /tmp/bench-rules.yaml --no-external 2>&1 | tee /tmp/bench-noext.log
   /usr/bin/time -l tga classify --force --rules /tmp/bench-rules.yaml 2>&1 | tee /tmp/bench-full.log
   ```

2. **Record results** in a new file:
   ```
   docs/trusty-git-analytics/regression-testing/v{VERSION}-{YYYY-MM-DD}.md
   ```

3. **Update the symlink**:
   ```bash
   cd docs/trusty-git-analytics/regression-testing
   ln -sf v{VERSION}-{YYYY-MM-DD}.md current.md
   ```

4. **Update the index** in this README (add a bullet under "Version snapshots").

5. **Update `crates/trusty-git-analytics/README.md`** Performance section with headline numbers.

## Snapshot Structure

Each snapshot `.md` file includes:

- **Header**: version, date, corpus identifier, hardware
- **Methodology**: what was run, how it was timed, key flags
- **Raw numbers table**: classify time, peak RSS, commits/sec, coverage
- **Method distribution**: per-tier counts with percentages
- **External-source efficiency**: JIRA cache hit ratio, error rate
- **Per-category breakdown**: top categories with counts
- **`--no-external` baseline comparison**: time delta, coverage delta
- **Anomalies**: unexpected findings
- **Reproducibility**: exact commands, version, env vars required

See [`v1.3.0-2026-05-27.md`](v1.3.0-2026-05-27.md) as the template example.

## Regression Thresholds

When a new snapshot is added, flag regressions if:
- Coverage drops ≥ 3 pp vs prior snapshot (same rules, same corpus)
- CPU throughput drops ≥ 10% vs prior snapshot
- Peak RSS increases ≥ 20% vs prior snapshot

---

**Reference corpus**: Duetto `~/Duetto/cto` (72,608 commits, 20 JIRA projects)
