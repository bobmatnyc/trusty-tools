# trusty-search performance results

This directory holds one dated JSON file per monitoring run. Files are append-only: a run adds a file, and a committed file is never edited. Raw per-call series stay in the gitignored `.trusty-mpm/perf/trusty-search/`. Only the summary is committed.

File name: `<YYYY-MM-DD>-<version>-<host-class>-<pass>.json`, for example `2026-10-22-0.61.0-mac-p0.json`.

## Rank labels

Indexes appear as `i01`, `i02`, `i03`, ranked by chunk count, largest first. Private index names, ids and paths are never committed. The label-to-index map stays in the gitignored query-set file.

## Envelope fields

Top level: `schema`, `kind`, `run_id`, `utc`, `pass`, `version`, `daemon_uptime_s_at_start`, `host_load_1min`, `run_solo`, `load_note`, `daemon_rss_kb_start`, `daemon_rss_kb_end`, `sidecar_rss_kb`, `embedder`, `env_confirmation`, `indexes`, `calls_used`, `timeouts`, `client_errors`, `fallbacks`, `log_events_since_restart`.

Each entry under `indexes` (`i01`, `i02`, ...): `chunks`, `size_bytes`, `cold_probe_ms`, `cold_probe_wall_ms`, `cold_probe_state`, `warm_per_mode`, `warm_pooled`, `fallback_ripgrep_calls`, `timeouts`, `vs_baseline`.

`warm_per_mode` has one entry per search mode (`lexical`, `semantic`, `hybrid`, `hybrid_rg_off`). Each entry holds `n` and `values_ms`.

## Percentiles

Percentiles use the nearest-rank method. Below n=20 a percentile is not a stable figure, so the file lists the raw values and marks pooled figures "indicative only".

## Cadence

- A daily passive rollup of the daemon logs.
- A live pass every Monday.
- An extra pass on each release and each daemon restart.

## Comparison rules

Never compare numbers from different hosts. Compare within one host class only, and read `host_load_1min`, `run_solo` and `embedder` before reading any difference. A run whose embedder or load differs from the baseline is not a like-for-like match.
