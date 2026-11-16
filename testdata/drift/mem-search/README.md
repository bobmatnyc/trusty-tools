# Memory and search drift test

Accuracy and latency check of the live trusty-search and trusty-memory daemons (goal ar-G05).

Run: `python3 -I scripts/drift_test_mem_search.py` (add `--dream` for the dream lane, `--no-record` to skip the results row, `--write-baseline` to replace `baseline.json`, `--selftest` for the offline checks, which run `scripts/test_drift_test_mem_search.py`). Any absent lane, missing golden file, failed seed or unverified delete is a finding with exit 2. At start the run deletes `drift-dream-test-*` palaces older than one hour that an earlier run leaked, and reports each.

- `golden.public.json`: 14 search queries on the public trusty-tools index.
- `golden.private.json` (NOT in this repo, which is public): 16 search queries on the `architect` and `cto` indexes, and 42 memory queries on the `supervisor` and `cto` palaces. Twelve of them (`"kind": "dated_ruling"`) target drawers that hold a dated, quoted owner ruling. It lives at `~/.trusty-tools/drift/mem-search/golden.private.json`; each run row records its sha256 prefix.
- `baseline.json`: aggregate metrics, cold shares, versions and index sizes of the baseline run.
- Run rows append to `runs.jsonl`, in the same directory as the private golden file.

## Lanes

- `search:as-called`: `search` with no `mode`, exactly as agents call it today.
- `search:best-mode`: `mode` set by target type: `text` when every expected target is prose (`.md`, `.mdx`, `.txt`, `.rst`), else `code`. It runs right after as-called on the same query, so its embeds are warm; its cold share shows that.
- `memory`: recall on test palaces, never on a live palace. `memory_recall` raises a served drawer's `access_count` (#11348), so the run creates one guarded test palace per source palace (`drift-dream-test-<utc>-<hex>`), copies the source drawers in (the source is only listed), recalls there through a live-to-test id map, and deletes every test palace in a `finally` block. The row records each delete: error, still listed, data dir present. `memory_recall` is refused on any palace the run did not create. `memory:dated-rulings` is the dated-ruling subset.
- `overall`: as-called search plus memory, the mix agents get.

Every lane reports recall@5, recall@10, MRR, p50, p95, degraded share and `embed_cold_share`.

Known leak: the daemon's user-scope rulings leg (`TRUSTY_MEMORY_RULINGS_PALACES`) folds rulings from a live palace into a recall on any palace and records an access on them. The run cannot switch that leg off; it counts served hits that are not in the test palace as `foreign_hits` and reports any as a finding.

## Findings

Printed, exit code 2: recall@10 drop over 0.05, p95 over 1.5x, degraded share up over 0.10, each against the baseline and the previous run, lane by lane.

The p95 check runs only when the lane's cold share is within 0.25 of the reference's. A cold query embeds; a warm one is served from the daemon's query-embed cache, so a swing of more than a quarter of the queries moves p95 by itself. A lane with no embed timing (memory has no query-embed cache) records `null` and compares against `null`. A skipped check is listed in the row's `checks_skipped` with the reason.

A first query against a parked index reloads it; warm-up latency is recorded separately and excluded from the metrics.

## Dream lane

`--dream` (`--dream-source <palace>`, default `supervisor`) reuses the memory lane's test palace for that source, measures the source's memory golden queries, runs ONE `palace_dream` pass, and measures again. The palace is deleted with the others. A `PalaceGuard` refuses every write tool and `memory_recall` on a palace this run did not create; `--selftest` checks the refusals.

Pass: no golden drawer lost (a dedup-merged drawer counts through its survivor), recall@10 drop within 0.05, duplicates not higher, and 5 dated rulings (picked once into `golden.private.json` as `dream_rulings`) keep their date and quoted words byte-identical. The row is a `"lane": "dream"` line in the same `runs.jsonl`; `baseline.json` holds it under `dream`.

Caveat: seeded drawers are all created at seed time, and consolidation only takes drawers older than `max_age_days`, so a fixture pass currently changes nothing. The lane flags that as a finding (`noop_pass`) instead of reporting a green verdict.
