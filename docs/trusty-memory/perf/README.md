# trusty-memory performance baselines

Dated, committed baseline snapshots of trusty-memory speed, effectiveness, storage and indexing. Each run writes one JSON file:

```
<YYYY-MM-DD>-<version>-<label>.json      e.g. 2026-10-17-0.36.0-evo.json
```

`label` is `evo` or `mac`. Raw series (10 s RAM samples, per-call latencies) stay in the gitignored `.trusty-mpm/perf/memory/`. Only their summary enters the JSON. The eval gate file `crates/trusty-memory/testdata/recall_eval/baseline.json` stays where it is. It is a gate, not a record.

## Envelope schema

Schema version 1. Shared with trusty-search so one reader parses both.

- `schema`: envelope version, `1`.
- `tool`: tool under test, `trusty-memory`.
- `version`: crate version from the pinned worktree's `Cargo.toml`.
- `commit`: `git rev-parse HEAD` of the pinned worktree.
- `label`: `evo` or `mac`.
- `host`: `{id, os, cpu, ram_gb}`.
- `date`: run date, `YYYY-MM-DD`.
- `corpus`: eval corpus or seed size used.
- `load`: `{before, after}`, 1-minute load average around the timed block.
- `residency`: `{cached, total}`, resident palaces against all palaces.
- `results`: list of measurements. Each entry is `{dimension, metric, unit, n, k, p50, p95, max, value, threshold, notes}`.
  - `dimension`: speed, effectiveness, storage or indexing.
  - `metric`: metric name.
  - `unit`: unit of the numbers (`us`, `ms`, `bytes`, ratio).
  - `n`: sample count.
  - `k`: cutoff, set on recall metrics only.
  - `p50`, `p95`, `max`: distribution figures, where the metric is a distribution.
  - `value`: single figure, where the metric is not a distribution.
  - `threshold`: the alert threshold the report names (see Alert thresholds).
  - `notes`: free text, such as "access counts start at zero".

## Suites

Run each suite from a committed clean linked worktree through `mpm-utils/scripts/evo`, with toolchain `+1.99.0`. The main checkout is refused. Log every run with `evo-gate-log --tag perf-baseline`. An EVO gate counts against the PM's one Rust slot.

| Test | File | Profile | Runs | Measures |
|---|---|---|---|---|
| `recall_eval_holds_the_recorded_baseline` and its red arm `recall_eval_goes_red_with_demotion_or_retirement_off` | `crates/trusty-memory/tests/recall_eval.rs` | debug (ranks are deterministic) | x1 | hit@1 and superseded-above-current over 32 queries; per-query ranks give MRR and hit@k |
| `recall_supersession_latency_profile` | `crates/trusty-memory/tests/recall_supersession.rs` | release | x5 | end-to-end `memory_recall` p50/p95 at 20,000 drawers, rulings leg off and on, plus `seeded_in` |
| `concurrent_perf` (all `#[ignore]` tests) | `crates/trusty-memory/tests/concurrent_perf.rs` | release | x3 | latency distribution under concurrent reads, read/write mix, burst and sustained load |
| `engine::retrieval::chunk_backfill_measure_tests::slice_defaults_and_recall_contention_measured` | `crates/trusty-memory/src/engine/retrieval/chunk_backfill_measure_tests.rs` | release | x3 | ONNX embed time per chunk, slice wall time, query-embed p50/p95/max idle and under a running slice |

Command shapes (from the plan, `.trusty-mpm/plans/memory-perf-analysis-plan.md` sections 2 and 3):

- Latency profile: `evo cargo +1.99.0 test -p trusty-memory --release --test recall_supersession recall_supersession_latency_profile -- --nocapture`. The test is inert unless `TRUSTY_RECALL_SUPERSESSION_PROFILE` is set. On EVO pass it with `--config 'env.TRUSTY_RECALL_SUPERSESSION_PROFILE="1"'`.
- `concurrent_perf`: `--release` with `--include-ignored --nocapture`.
- Slice/embed measure: `--release --lib -- --ignored --exact <full test path above>`.
- Eval: debug build, `--nocapture`, `--include-ignored`. Both recall eval tests are `#[ignore]`. Without `--include-ignored` the run executes 0 eval tests and prints no ranks. Stderr lines `recall-eval [mechanisms on] qNN rank=...` give the ranks.

Report the median of the runs. Each run uses fresh temp dirs and no daemon.

Record the toolchain (`rustc -V` and the `+toolchain` used) and the exact command line of each suite in the JSON, in `notes`. The 2026-10-17 file has neither.

## Comparison rules

- Compare EVO to EVO only. EVO is a CPU embedder on a different machine.
- Never compare EVO figures to the Mac host or to the Mac figures in [#9280](https://github.com/bobmatnyc/trusty-tools/issues/9280).
- Use the median of the runs, not the best run.
- Every run starts from fresh temp dirs, so access counts start at zero. Record that in `notes`.
- Never benchmark against the live daemon. `memory_recall` writes `drawer_access`, which feeds `effective_importance` and changes ranking.
- Discard a block whose idle gate failed (1-minute load average of 2 or more before or after). Never compare a discarded block.

## Alert thresholds

Each report names its threshold in `threshold`.

- Eval: hit@1 below 27/32, or any superseded-above-current, fails the build. That is a test failure, not an analysis finding.
- Latency and indexing: p95, `seeded_in` or an embed time at +25% or more over the pinned baseline is a warn; +50% or more is an alert. Use the median of the runs, same host and toolchain. These levels come from the Architect ruling of 2026-10-23 on [#10210](https://github.com/bobmatnyc/trusty-tools/issues/10210) (Q5). They replace the earlier 10% latency and 25% indexing lines. `scripts/perf_compare.sh` applies them (see Compare and status file).
- Storage: weekly growth more than 2x the trailing 4-week median, a single palace over 500 MB, or `reclaimable_bytes` above 25% of `kg.redb`.
- Storage, HIGH: missing vectors above 0 with a clean alias audit (findable-data loss).
- Freshness above 1 s: no threshold until a baseline exists.

A crossing goes to the Architect. The analysis files no issues and posts no comments.

EVO p95 is noisy under fleet load, so a latency crossing can fire on noise. One run on 2026-10-17 reached p95 28 ms against a 16.4 ms median. Check `load` in the envelope before reading a crossing.

## Compare and status file

`scripts/perf_compare.sh BASELINE CURRENT` compares two schema-1 envelopes of the same `tool` and `label`. It grades `p95` where the baseline row has one, and `value` for `indexing` rows with no p95 (`seeded_in`, embed times). Effectiveness rows are not graded; the eval gate is a test. The delta is `(current - baseline) / baseline` in percent, rounded to 0.01. Both levels include their boundary: exactly +25.00% is a warn and exactly +50.00% is an alert. The comparison fails closed. A baseline row missing from the current envelope is listed under `missing`, and a current figure that is null or not a number is listed under `unparsed`; either makes the verdict at least a warn, and an alert still outranks it. A baseline figure that is zero, negative, NaN or infinite, a current figure that is negative, NaN or infinite, and a comparison with no graded row are errors. The report is JSON on stdout. Exit codes: 0 ok, 10 warn, 11 alert, 12 error (missing or malformed file, schema other than 1, tool or label mismatch, or one of the errors above).

`scripts/perf_status_write.sh` records the latest run per tool in the gitignored `.trusty-mpm/perf/status.json` that the Architect poller reads. Pass `--compare <report>` for a compared run, or `--verdict failed|skipped --reason <text>` for a run that produced no comparison. Each entry carries `ts`, `verdict` (ok, warn, alert, failed or skipped), the graded metrics with their baseline deltas, and `skipped_streak`. The reader warns on the first skipped run and alerts on the second in a row. The writer refuses a path inside the repository that git does not ignore.

`scripts/perf_compare_selftest.sh` runs both scripts against fixtures in `scripts/test-data/perf-compare/`, cut from the two committed EVO files. Compared the same way, the 2026-10-23 file is an alert against 2026-10-17: `seeded_in` is +56.48%.

## Live collector (read-only)

`scripts/memory_perf_collect.sh` takes the storage, indexing and dream-score rows from a running or stopped trusty-memory data root and prints one envelope (schema 1) on stdout. It adds two fields to the envelope: `idle_gate` (`passed`, `failed` or `skipped`) and `sampled_palaces`. Palace rows carry `palace` (a size-rank label `p01`, `p02`, ...) and, for file rows, `file` (a basename).

Read-only guarantee. The script reads file sizes (`stat`, `du -sk`), runs `trusty-memory palace stats <name> --json` serially, calls `memory.status` once over the socket, and reads each sampled palace's `dream_stats.json`. It never calls `memory_recall`, `memory_recall_all`, `palace_embed_sweep` or any write, compact, dream or reembed verb, and never starts or stops a daemon.

Public output. The JSON holds no palace name, project name, host path, user name or host name. The host is `label`, OS name and version, CPU model and RAM GB.

Idle gate. A 1-minute load of 2 or more before the run is a refusal (exit 3). A load of 2 or more after the run marks `idle_gate: "failed"`; discard that run. `--skip-idle-gate` marks `"skipped"` and is for sandbox checks only.

| Flag | Meaning |
|---|---|
| `--label mac\|evo` | host label (default by OS) |
| `--top N` | palaces sampled, largest by disk bytes (default 10) |
| `--data-root DIR` | data root, last component `trusty-memory` (env `MEMORY_PERF_DATA_ROOT`) |
| `--socket PATH` | daemon socket (env `MEMORY_PERF_SOCKET`; default `<data-root>/trusty-memory.sock`) |
| `--bin PATH` | `trusty-memory` binary (env `MEMORY_PERF_BIN`) |
| `--out FILE` | also write the JSON to FILE |
| `--commit SHA`, `--tool-version V`, `--corpus NAME` | envelope fields; defaults are the script checkout's HEAD, the daemon's version, `live` |

Needs `jq`, `nc`, `du`, `stat`, `awk` and `trusty-memory`. With no daemon on the socket, `residency` is null and the file and `palace stats` rows still run.

Sandbox check: start a daemon with `scripts/sandbox_memory_daemon.sh`, then pass its `data/trusty-memory` dir as `--data-root`.

Metric notes. `dream_recall_score_before` and `_after` are the mean top-3 cosine of 8 fixed queries, not a correctness measure. `dream_duration_ms` is bimodal because the embed passes are skipped when the drawer set is unchanged. `commit` is the script checkout's HEAD, not the daemon build.
