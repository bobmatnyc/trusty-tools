# Regression Testing & Benchmark Snapshots

This folder contains snapshots of [trusty-review](../../../crates/trusty-review/) review-quality and performance benchmarks, one per release that is measured.

## Purpose

Snapshots in this folder provide:
- **Baseline data** for each release: review-finding precision/recall, verdict accuracy, end-to-end latency, token cost
- **Historical context**: Quality and performance trajectory across releases
- **Regression detection**: Spot anomalies when new releases ship
- **Investigation artifacts**: Raw results for deep-dive analysis when regressions are detected

## Methodology

All benchmarks should use a consistent setup so snapshots are comparable release-over-release:

**Corpus**: a fixed set of reference PRs (or synthetic diffs) with known expected findings.

**Comparators** (where applicable):
- The previous trusty-review release — primary regression comparator
- A raw "LLM-only, no context" review — to measure the value added by search/analyze context

**Metrics** (adapt to the suite that lands):
- **Finding precision / recall**: Fraction of surfaced findings that are real / fraction of seeded issues caught
- **Verdict accuracy**: Agreement of the structured verdict with the expected outcome
- **Latency**: End-to-end review time (diff fetch + context retrieval + LLM call), reported as mean / p50 / p95
- **Token cost**: Prompt + completion tokens per review (proxy for $ cost)

## File Naming Convention

```
v{VERSION}-{YYYY-MM-DD}.md
```

Examples:
- `v0.3.6-2026-06-08.md` — v0.3.6 benchmarked on June 8, 2026
- `v0.4.0-2026-07-01.md` — v0.4.0 benchmarked on July 1, 2026

Alternate-corpus baselines (e.g. a synthetic-diff suite) live alongside the
version snapshots with a descriptive name, e.g.
`synthetic-corpus-baseline-{YYYY-MM-DD}.md`.

## Snapshot Index

_No snapshots recorded yet._

### Latest

`current.md` — symlink to the latest version snapshot. Create it when the first
snapshot lands:

```bash
ln -sf v{VERSION}-{YYYY-MM-DD}.md docs/trusty-review/regression-testing/current.md
```

## Adding a New Snapshot

When benchmarking a release:

1. **Run the review benchmark suite** against the fixed reference-PR corpus.
2. **Record results** in a new file following the naming convention:
   ```
   docs/trusty-review/regression-testing/v{VERSION}-{YYYY-MM-DD}.md
   ```
3. **Update the `current.md` symlink** to point at the new snapshot.
4. **File a regression bug** if any primary quality metric (precision, recall, or
   verdict accuracy) regresses materially versus the prior release.

## Snapshot Structure

Each snapshot `.md` file should include:

- **Header**: Version, date, corpus state (PR count / diff size, model id used)
- **Summary table**: Per-PR expected vs. actual findings and verdict
- **Aggregate metrics**: Precision, recall, verdict accuracy, latency stats, token cost
- **Anomalies**: Any unexpected per-PR results or performance patterns
- **Relevant PRs/fixes**: Links to issues/PRs that landed in this release
- **Cross-links**: Link to related tickets and the sessions narrative for the release

---

**Related**: [Session Summaries Index](../sessions/README.md) | [Research & Decision Documents Index](../research/README.md) | [Spec](../spec/)
