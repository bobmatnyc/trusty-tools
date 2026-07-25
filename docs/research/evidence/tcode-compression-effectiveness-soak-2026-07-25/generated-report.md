## Verdict

**PASS** — both targets met

Targets (epic #2343): working context >= 60% at all times; `compaction_events == 0`.

## Source

- File: `docs/research/evidence/tcode-compression-effectiveness-soak-2026-07-25/compression.jsonl`
- Session id filter: `7ae5f495-9931-4025-8de6-672f37489646`
- Total events scored: 28

## Event counts per surface

| Surface | Count |
|---|---|
| `tcode-cadence` | 28 |

## Compression-ratio distribution (`tcode-cadence`)

| Stat | Value |
|---|---|
| Events | 28 |
| Min | 0.3010 |
| Median | 0.5340 |
| P95 | 0.7260 |
| Max | 0.9990 |

(`ratio` = tokens_after / tokens_before; lower is more aggressive compression.)

## Working-context floor

- Minimum observed: **95%** (28 samples)
- Never dropped below 60% at any sample.

| # | working_context_pct_after | |
|---|---|---|
| 0 | 97% |
| 1 | 99% |
| 2 | 99% |
| 3 | 99% |
| 4 | 99% |
| 5 | 96% |
| 6 | 97% |
| 7 | 97% |
| 8 | 99% |
| 9 | 99% |
| 10 | 99% |
| 11 | 99% |
| 12 | 95% |
| 13 | 96% |
| 14 | 96% |
| 15 | 98% |
| 16 | 98% |
| 17 | 98% |
| 18 | 98% |
| 19 | 95% |
| 20 | 96% |
| 21 | 96% |
| 22 | 98% |
| 23 | 98% |
| 24 | 98% |
| 25 | 98% |
| 26 | 95% |
| 27 | 96% |

## Threshold compaction (`tcode-threshold`)

0 threshold-compaction events fired. Target met.
