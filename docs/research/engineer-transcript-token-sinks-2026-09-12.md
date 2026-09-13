# Engineer transcript input-token sinks (2026-09-12)

Two live `rust-engineer` JSONL transcripts, analyzed by summing `usage` on
assistant turns and byte-sizing `tool_result` content. Still growing during
capture; numbers are the last snapshot.

- T1 `abe9494fbf2104506`: tm doctor / auto-memory (#7685), 309 turns.
- T2 `a03f546edc60c0a79`: trusty-search `exact.rs` rewrite (#7675), 149 turns.

## 1. Turn accounting

| | turns | floor/turn | sum input | floor×turns | growth |
|---|---|---|---|---|---|
| T1 | 309 | 91,145 | 83,109,654 | 28,163,805 (33.9%) | 54,945,849 (66.1%) |
| T2 | 149 | 90,504 | 35,591,535 | 13,485,096 (37.9%) | 22,106,439 (62.1%) |

Floor (~91K, near-identical both transcripts) = resident prompt (system,
tools, BASE-AGENT, CLAUDE.md, skills), paid every turn regardless of task.
Growth = accumulated tool-result/message history repeated via cache_read.

## 2. Tool-result categories (~tokens / calls)

T1 is Bash-heavy — `sed -n`/`cat -n`/`grep` used instead of Read/Grep. T2 is
Read-tool-heavy, correctly using offset/limit.

| category | T1 | T2 |
|---|---|---|
| file reads (bash sed/cat) | 40,535 / 36 | 2,422 / 6 |
| file reads (Read tool) | 8,894 / 7 | 28,639 / 17 |
| grep/search (bash) | 19,311 / 60 | 7,252 / 25 |
| git/gh | 1,172 / 6 | 5,679 / 10 |
| other bash | 4,657 / 18 | — |
| cargo build/test/clippy | 220 / 15 | 66 / 10 |

## 3. Repetition, compression, divert

- **Repetition**: T1 negligible — one file read twice (8,080B); its large
  `sed -n` slices are distinct line ranges, not re-reads. T2: 4 files read
  2-3x — `exact.rs` (3x, 22,339B, first a full-file read then two
  *overlapping* offset/limit reads = genuine duplication), `exact_match_
  floor.rs` (3x, 18,556B, sequential non-overlapping = legitimate
  pagination), `lanes.rs` (2x, 18,138B), `trusty-common/bm25.rs` (3x,
  17,011B). No Bash command ran >2x either transcript; no cargo test output
  exceeded 20KB.
- **Compression**: both redirect cargo output to a file + `echo EXIT=$?` per
  BASE-AGENT's gate protocol — tool_result itself is tiny (T1: 880B/15
  calls; T2: 267B/10 calls). Already working; no further recovery here.
- **Divert**: T1 0 `tm divert` hits, 5/7 Reads used offset/limit — divert
  mostly sidestepped via Bash `sed -n`, which evades Read-tool tracking
  entirely. T2: 2 `tm divert` hits, 14/17 Reads used offset/limit — correct
  behavior, though it produced the overlapping `exact.rs` re-read above.

## 4. Output tokens

T1: 130,230. T2: 72,833.

## Ranked levers (tokens recoverable, combined)

1. **Floor reduction (#7683 allowlists, ~40K/turn target): ~18.3M tokens.**
   `40,000×309 + 40,000×149` = 12.36M + 5.96M. Dwarfs every other lever
   because it multiplies by every turn, not just turns after one tool call.
2. **Bash-as-Read/Grep replacement (T1): ~60K raw tokens** (162,143 +
   77,245 bytes, 96 calls) that evade Read-tool dedup/divert tracking. Real
   value is preventing the double-read this invisibility causes, not bytes.
3. **De-duplicated re-reads (T2): ~19K raw tokens** (76,044 bytes, 4 files),
   of which only `exact.rs` is genuine waste (1 file, not 4). Compounds
   across the ~75 remaining turns after the second read — effective cost is
   materially larger than 19K, but this pass did not model the per-turn
   multiplier precisely enough to give an exact figure.
4. **Grep result caps: ~26.5K raw tokens** (85 calls, both transcripts) — no
   single result was an outlier; a cap trims the tail, not the bulk.
5. **Cargo compression: already implemented, ~0 additional recovery.**

## Improvement recommendations

- **Symptom**: T1 used Bash `sed -n`/`cat -n`/`grep` for 96 of ~135 Bash
  calls instead of Read/Grep, for in-worktree file inspection and search.
- **Cause**: no instruction steers an engineer toward Read/Grep over Bash
  text tools when both work; BASE-AGENT's divert guidance is written in
  terms of Read-tool offset/limit, so a Bash-sed agent never triggers it.
- **Change**: add a line to BASE-AGENT or rust-engineer preferring Read
  (offset/limit) and Grep over Bash `sed -n`/`cat -n`/`grep` for in-repo
  inspection — the Bash path bypasses Read-tool dedup/divert tracking and
  produced T2's genuine `exact.rs` double-read once offset/limit was in play.
- **Evidence**: T1 categories above (162,143+77,245 bytes / 96 Bash calls
  vs. 35,577 bytes / 7 Read calls); T2's `exact.rs` triple read (section 3).

## Prompt feedback

Separating genuine duplication from legitimate pagination required reading
actual event content beyond aggregate stats — a necessary second pass, not
avoidable from usage/byte totals alone. Framing the repetition ask as "same
offset/limit twice" vs. "overlapping ranges" vs. "sequential chunks" up front
would get the taxonomy right in one script pass instead of two.
