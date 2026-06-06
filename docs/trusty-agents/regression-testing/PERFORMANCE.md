# Performance Telemetry

Every `open-mpm --workflow <name>` invocation emits one JSON run file plus a
one-line summary into this directory. This is how we compare builds and track
prompt-caching wins over time.

Output layout:

```
docs/performance/
├── README.md              <- this file
├── analyze.py             <- CLI summary across all runs
├── runs.log               <- append-only one-line summary per run
└── runs/
    └── 20260422-173130-build42.json   <- full per-run record
```

## Run-file schema (`runs/<stamp>.json`)

```json
{
  "build": 42,
  "version": "0.1.0",
  "workflow": "prescriptive",
  "task_preview": "Write a Python script that …",
  "started_at": "2026-04-22T17:31:30Z",
  "total_duration_ms": 18422,
  "phases": [
    {
      "name": "research",
      "duration_ms": 4100,
      "prompt_tokens": 3412,
      "completion_tokens": 812,
      "cache_read_tokens": 0,
      "cache_creation_tokens": 2800,
      "cost_usd": 0.0277
    }
  ],
  "totals": {
    "prompt_tokens": 0,
    "completion_tokens": 0,
    "cache_read_tokens": 0,
    "cache_creation_tokens": 0,
    "cost_usd": 0.0
  }
}
```

Field notes:

- `build` — monotonic counter from `.open-mpm/build.json`. Matches the banner
  printed at startup so you can correlate logs and perf records.
- `task_preview` — first 120 chars of the task text, ellipsized. Full task is
  deliberately *not* captured (would bloat the file and risk leaking secrets).
- `cache_read_tokens` / `cache_creation_tokens` — Anthropic-specific
  (issue #50). Always `0` for non-Anthropic models.
- `cost_usd` — computed from a hard-coded pricing table in `src/perf.rs`;
  update that table when Anthropic/OpenRouter change prices.

## Summary log (`runs.log`)

One tab-separated line per run:

```
<iso_started_at>\tbuild=<N>\tworkflow=<name>\tdur_ms=<N>\tprompt=<N>\tcompletion=<N>\tcache_r=<N>\tcache_w=<N>\tcost_usd=<N>
```

Useful for `tail -f` during a session or grepping by build number.

## Analysis

`analyze.py` prints a compact summary table across all `runs/*.json`:

```bash
python docs/performance/analyze.py
# or
python docs/performance/analyze.py --workflow prescriptive
```

No third-party deps — stdlib only.

## When to look at these files

- **After every workflow run**: scan `runs.log` for the latest row; look at
  per-phase `duration_ms` and `cost_usd` in the JSON for the breakdown.
- **Build-over-build regressions**: `analyze.py` sorts by build number; a
  sudden jump in `total_ms` or `total_cost_usd` for the same workflow means
  prompts got larger, a phase regressed, or a model switch landed.
- **Prompt caching effectiveness**: `cache_read_tokens` climbing vs.
  `cache_creation_tokens` on repeated workflow runs with Anthropic models is
  the #50 acceptance signal.

## Related

- Build counter: `src/build_info.rs` and `.open-mpm/build.json`
- Collector implementation: `src/perf.rs`
- Wiring: `src/workflow/engine.rs` (`PerfCollector::new` → `record_phase` →
  `flush`)
