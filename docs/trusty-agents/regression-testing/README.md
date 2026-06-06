# open-mpm — Regression Testing

Performance baselines, bake-off comparisons, and the per-run telemetry tooling
for open-mpm. This is the canonical `regression-testing/` subdir for the crate
(see the workspace [`CLAUDE.md`](../../../CLAUDE.md) documentation conventions).

## Contents

| File | What's here |
|---|---|
| [PERFORMANCE.md](./PERFORMANCE.md) | Per-run telemetry: run-file JSON schema, `runs.log` format, and how to read the numbers. The reference for the telemetry pipeline. |
| [baseline.md](./baseline.md) | Performance baseline measurements. |
| [bakeoff-comparison.md](./bakeoff-comparison.md) | Bake-off comparison results across builds/models. |
| `analyze.py` | Stdlib-only CLI that prints a compact summary across all `runs/*.json`. Run `python analyze.py` (optionally `--workflow <name>`). |
| `runs.log` | Append-only, one tab-separated line per workflow run. |

## How telemetry is produced

Every `open-mpm --workflow <name>` invocation emits one JSON run file plus a
one-line summary. At runtime these land under the **consuming project's**
`<cwd>/docs/performance/` directory (`runs/<stamp>.json` + `runs.log`); the
schema and field notes are documented in [PERFORMANCE.md](./PERFORMANCE.md).
The files in this directory are curated baselines and the analysis tooling,
kept alongside the spec for cross-release tracking.

For **cross-release performance tracking** across all trusty-* crates, see
GitHub issue [#129](https://github.com/bobmatnyc/trusty-tools/issues/129).

---

[← Back to open-mpm docs index](../README.md)
