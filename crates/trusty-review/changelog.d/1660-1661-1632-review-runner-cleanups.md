Fixed
- The review runner rendered the full noise-filtered diff twice (once unbounded just to measure its length, once bounded for the prompt) — now renders once; the map-reduce decision reuses the bounded render's own truncation marker (Refs #1660).
- The map-reduce branch assigned `ReviewStatus::Degraded` from two independent sites (partial coverage, opted-out context dependency) — consolidated to one assignment so a future finer-grained status can't be silently clobbered by whichever site ran second (Refs #1661).
- The calibration harness's `run_pipeline_for_entry` built its own `GithubClient`/token instead of reusing the runner's shared `resolve_diff_token` helper — now routes through the same funnel, removing an auth-path divergence risk (Refs #1632).
- `compute_metrics` panicked via `assert_eq!` on a corpus/results length mismatch — now returns a `Result` error instead (Refs #1632).
