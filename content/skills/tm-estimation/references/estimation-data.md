# tm-estimation — measured rows

Raw rows behind the ratio table in `tm-estimation.md`. Append rows when a plan
closes; recompute the class medians and ranges from this file. Estimates are
engineer-hours at 8 h per engineer-day. Actual is agent wall-clock hours,
human waits excluded. Ratio = actual ÷ engineer estimate.

## Source 1 — trusty-browser Rust port, 2026-11-17/18

Plan: `docs/plans/trusty-browser-rust-port.md`. The Python package was a
byte-level oracle for phases 0–7.

| Phase | Class | Est. (h) | Start (UTC) | End (UTC) | Actual (h) | Ratio | Cause of gap |
|---|---|---|---|---|---|---|---|
| S Keychain spike | spike | 4 | 11-17T20:10Z | 11-17T20:23Z | 0.22 | 5.5% | Fully scripted; prompts surfaced as timeouts |
| 0 | port | 16 | 11-17T20:24Z | 11-17T20:37Z | 0.20 | 1.25% | Scripted capture, warm target; ~2 min Keychain dialog incident |
| 1 | port | 32 | 11-17T20:38Z | 11-17T20:52Z | 0.24 | 0.75% | Translation against byte oracle |
| 2 | port | 48 | 11-17T20:53Z | 11-17T21:16Z | 0.37 | 0.77% | Seeded oracle; replay matched first run |
| 3 | port | 48 | 11-17T21:17Z | 11-17T21:37Z | 0.34 | 0.71% | Byte-oracle translation, no waits |
| 4 | port | 24 | 11-17T22:04Z | 11-17T22:22Z | 0.30 | 1.25% | Python CLI as oracle; green first run |
| 5 | port | 32 | 11-17T21:43Z | 11-17T22:03Z | 0.33 | 1.03% | Fake socket capture; one truncation fix |
| 6 (non-live) | port | 32 | 11-17T22:23Z | 11-17T22:32Z | 0.15 | 0.47% | Suite already deterministic |
| 7 | port | 16 | 11-17T22:41Z | 11-17T22:45Z | 0.07 | 0.44% | Metadata and docs only |
| CR critic round | review | 2 | 11-17T22:33Z | 11-17T22:37Z | 0.06 | 4 min | Read-only; APPROVE |
| FX fix round | fix | 1 | 11-17T22:36Z | 11-17T22:41Z | 0.08 | 8% | Scoped fixes, warm cache |
| SEC flag fix | fix | 1 | 11-17T21:38Z | 11-17T21:43Z | 0.08 | 8% | Small scoped fix |
| SG Developer ID signing | new design | 2 | 11-17T23:13Z | 11-17T23:18Z | 0.10 | 5% | Seam and fake already in place |
| SG2 sign mcp sibling | fix | 1 | 11-17T23:19Z | 11-17T23:26Z | 0.10 | 10% | Small |
| CO cutover | live ops | 4 | 11-17T23:26Z | 11-18T03:57Z | 0.17 agent + 4.35 wait | 4.3% | Unplanned Keychain dialog, owner away |
| RD README + comparison | docs | 3 | 11-18T08:06Z | 11-18T08:08Z | 0.04 | 1.3% | One-pass write from source |
| SK two-profile soak | timed run | 4 | 11-18T14:59Z | 11-18T15:29Z | 0.50 | n/a | 30 min soak by design |

## Source 2 — trusty-computer phase 2, 2026-11-18

Plan: `docs/plans/trusty-computer-phase2.md` on `feat/trusty-computer`. No
oracle beyond a ~400-line spike; live AX checks.

| Phase | Class | Est. (h) | Agent est. (h) | Start (UTC) | End (UTC) | Actual (h) | Ratio | Cause of gap |
|---|---|---|---|---|---|---|---|---|
| P plan | plan | 1 | n/a | 11:35Z | 11:40Z | 0.08 | 8% | Sources already local |
| 0 | new design | 8 | 0.2 | 11:40Z | 11:50Z | 0.15 | 1.9% | Small spike oracle, fast build |
| 1 | new design | 12 | 0.25 | 11:50Z | 11:54Z | 0.07 | 0.58% | Code written in phase 0 window |
| 2 | new design | 16 | 0.45 | 11:55Z | 12:09Z | 0.24 | 1.5% | Spike gave protocol; live click blocked |
| 3 | new design | 20 | 0.4 | 12:14Z | 12:41Z | 0.45 | 2.25% | 22-variant fail-open sweep, one hung variant |
| 4 | new design | 12 | 0.3 | 12:42Z | 12:55Z | 0.22 | 1.8% | Phase 3 rig carried the flow |
| 5 | new design | 12 | 0.25 | 12:55Z | 12:59Z | 0.08 | 0.67% | Work already done in phase 4 |
| 6 | new design | 8 | 0.2 | 13:00Z | 13:12Z | 0.20 | 2.5% | Modules ported 1:1 from trusty-browser |
| 7 | new design | 12 | 0.5 | 13:12Z | 13:50Z | 0.63 | 5.25% | Two review/fix cycles; screen-lock wait not counted |
| Total | | 100 | 2.55 | | | 2.04 | 2.0% | |

Review rounds inside phase 7: critic 0.08 h (BLOCK), delta 0.03 h (WARN),
delta 0.02 h (APPROVE). Fix rounds: 0.26 h and 0.13 h.

## Source 3 — other tasks, 2026-11-18

| Task | Class | Est. | Agent est. (h) | Actual (h) | Ratio | Note |
|---|---|---|---|---|---|---|
| issue-AC plan | plan | 0.5 h | n/a | 0.02 | 4% | |
| issue-AC build | new design | 11 eng-h | 0.7 | 0.9 | 8.2% | 1.29× agent est. |
| AC fix round | fix | 1 h | n/a | 0.82 | 82% | |
| matcher fix | fix | 0.25 h | n/a | 0.31 | 124% | |
| #11470 build | new design, scope grew | 6 eng-h | 0.3 | 1.83 | 30.5% | 6.1× agent est.; scope grew three times |
| #11470 fix round | fix | 1 h | n/a | 0.12 | 12% | |
| Cua spike | spike | 24 h | n/a | 0.63 | 2.6% | |
| gate inventory | plan | 1 h | n/a | 0.11 | 11% | |
| factory plan | plan | 0.5 h | n/a | 0.02 | 4% | |
| tm-estimation skill | docs | 4 h | 0.1 | 0.19 | 4.8% | 1.9× agent est.; worktree-guard retries |

## Human waits

| Wait | Length (h) | When | Cause |
|---|---|---|---|
| Keychain dialog | 4.35 | 11-17T23:35Z–11-18T03:56Z | Migrate step raised a dialog; owner away; service down |
| Screen lock | 1.95 | 11-18T11:27Z–13:24Z | Locked screen blocks AX live checks |
| GitHub rate limit | ~1 | 11-18 | Bulk `gh` calls |

## Class summary

| Class | Rows | Ratios, sorted | Median | Range |
|---|---|---|---|---|
| Port / byte oracle | 8 | 0.44, 0.47, 0.71, 0.75, 0.77, 1.03, 1.25, 1.25 % | 0.76% | 0.44–1.25% |
| New design, live checks | 10 | 0.58, 0.67, 1.5, 1.8, 1.9, 2.25, 2.5, 5.0, 5.25, 8.2 % | 2.1% | 0.58–8.2% |
| Spike, plan or docs | 7 | 1.3, 2.6, 4, 4, 5.5, 8, 11 % | 4% | 1.3–11% |
| Small fix round | 6 | 8, 8, 10, 12, 82, 124 % | 11% | 8–124% |
| Review round | 4 | 1.2, 1.8, 3.6, 4.8 min | 2.7 min | 1–5 min |

Excluded: #11470 build (scope growth, 30.5%), CO cutover (dominated by a human
wait), SK soak (fixed length).
