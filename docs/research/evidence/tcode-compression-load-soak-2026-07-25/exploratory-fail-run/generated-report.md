## Verdict

**FAIL** — working context dropped below 60% at 21 sample(s) (floor: 48%)

Targets (epic #2343): working context >= 60% at all times; `compaction_events == 0`.

## Source

- File: `/private/tmp/claude-502/-Users-masa-trusty-mpm-projects-bobmatnyc-trusty-tools--base--worktrees-2eb72dca-de08-481b-8dfa-22ab7f81b1f9/66306ece-6625-43d4-ad98-f8f0a322c7a7/scratchpad/evidence-final/exploratory-fail-run/telemetry/compression.jsonl`
- Session id filter: `(none — whole file)`
- Total events scored: 73

## Event counts per surface

| Surface | Count |
|---|---|
| `tcode-cadence` | 73 |

## Compression-ratio distribution (`tcode-cadence`)

| Stat | Value |
|---|---|
| Events | 73 |
| Min | 0.3392 |
| Median | 0.3437 |
| P95 | 1.0000 |
| Max | 1.0000 |

(`ratio` = tokens_after / tokens_before; lower is more aggressive compression.)

## Working-context floor

- Minimum observed: **48%** (73 samples)
- **21 sample(s) dropped below 60%** — flagged in the table below.

| # | working_context_pct_after | |
|---|---|---|
| 0 | 71% |
| 1 | 61% |
| 2 | 49% **BELOW TARGET** |
| 3 | 99% |
| 4 | 71% |
| 5 | 61% |
| 6 | 49% **BELOW TARGET** |
| 7 | 71% |
| 8 | 61% |
| 9 | 49% **BELOW TARGET** |
| 10 | 71% |
| 11 | 99% |
| 12 | 61% |
| 13 | 49% **BELOW TARGET** |
| 14 | 71% |
| 15 | 61% |
| 16 | 49% **BELOW TARGET** |
| 17 | 71% |
| 18 | 61% |
| 19 | 99% |
| 20 | 49% **BELOW TARGET** |
| 21 | 71% |
| 22 | 61% |
| 23 | 49% **BELOW TARGET** |
| 24 | 71% |
| 25 | 61% |
| 26 | 49% **BELOW TARGET** |
| 27 | 99% |
| 28 | 71% |
| 29 | 61% |
| 30 | 49% **BELOW TARGET** |
| 31 | 99% |
| 32 | 71% |
| 33 | 61% |
| 34 | 49% **BELOW TARGET** |
| 35 | 71% |
| 36 | 61% |
| 37 | 49% **BELOW TARGET** |
| 38 | 71% |
| 39 | 99% |
| 40 | 61% |
| 41 | 49% **BELOW TARGET** |
| 42 | 71% |
| 43 | 61% |
| 44 | 49% **BELOW TARGET** |
| 45 | 71% |
| 46 | 61% |
| 47 | 99% |
| 48 | 48% **BELOW TARGET** |
| 49 | 71% |
| 50 | 61% |
| 51 | 48% **BELOW TARGET** |
| 52 | 71% |
| 53 | 61% |
| 54 | 48% **BELOW TARGET** |
| 55 | 98% |
| 56 | 71% |
| 57 | 61% |
| 58 | 48% **BELOW TARGET** |
| 59 | 98% |
| 60 | 71% |
| 61 | 61% |
| 62 | 48% **BELOW TARGET** |
| 63 | 71% |
| 64 | 61% |
| 65 | 48% **BELOW TARGET** |
| 66 | 71% |
| 67 | 98% |
| 68 | 61% |
| 69 | 48% **BELOW TARGET** |
| 70 | 71% |
| 71 | 61% |
| 72 | 48% **BELOW TARGET** |

## Threshold compaction (`tcode-threshold`)

0 threshold-compaction events fired. Target met.
