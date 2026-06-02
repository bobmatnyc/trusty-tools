# Regression Testing & Integration-Test Snapshots

This folder contains snapshots of trusty-code integration-test and
compatibility-test results, one per measured release. Cross-release tracking
will be established in a dedicated GitHub tracking issue once tcode ships its
first release.

## Purpose

Snapshots in this folder provide:

- **Baseline data** per release — compatibility pass/fail counts, config-loader
  correctness, permission-model accuracy, MCP bridge round-trip latency
- **Historical context** — test trajectory across releases
- **Regression detection** — spot correctness regressions when new releases ship
- **Investigation artifacts** — raw results for deep-dive analysis when
  regressions are detected

## Naming Convention

```
v{VERSION}-{YYYY-MM-DD}.md
```

Examples:

- `v0.1.0-2026-08-01.md` — v0.1.0 integration suite run on 2026-08-01
- `v0.2.0-2026-09-15.md` — v0.2.0 run on 2026-09-15

## Snapshot Index

*(No snapshots yet. The first snapshot will be added when tcode ships Phase 0
and acquires an integration-test suite — see extraction phases in epic
[#587](https://github.com/bobmatnyc/trusty-tools/issues/587).)*

## Snapshot Structure

Each snapshot `.md` file should include:

- **Header** — version, date, test-suite revision, environment
- **Summary table** — test suite sections × (pass count, fail count, skipped)
- **Aggregate metrics** — overall pass rate, regression count vs. prior release
- **Notable failures / regressions** — any unexpected results and links to filed
  tickets
- **Relevant PRs/fixes** — links to issues/PRs that landed in this release
- **Cross-links** — tracking issue, related tickets

## Adding a New Snapshot

When running the integration suite for a release:

1. Run the tcode integration tests:
   ```bash
   cargo test -p trusty-code -- --include-ignored --nocapture
   ```

2. Record results in a new file following the naming convention.

3. Append a comment to the tracking issue with a one-line summary and a link to
   the snapshot file.

4. File a regression bug if any previously-passing compatibility tests begin
   failing.

---

**Related**: [Research & Decision Documents Index](../research/README.md) | [Session Summaries Index](../sessions/README.md)
