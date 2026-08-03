Fixed

- **`SubprocessAnalyzeClient`'s health check ignored degraded-but-serving,
  permanently blocking the review gate** (closes
  [#4440](https://github.com/bobmatnyc/trusty-tools/issues/4440)).
  - The MCP `review_pr` / `serve` path uses `SubprocessAnalyzeClient`, whose
    `health()` probed trusty-search's `/health` and tested `status == "ok"` as a
    literal string. trusty-search latches `status: "degraded"` for its entire
    process lifetime once warm boot skips any index (the underlying
    `degraded_by_timeout` / `degraded_by_tcc` counters are never decremented), so
    `has_analysis()` returned `false` forever and `pipeline::context_gate` skipped
    every review with "trusty-analyze unreachable/not-ready" — against a daemon
    that was up, embedder-ready and answering queries normally.
  - This was a duplicated health check that missed a fix applied to its twin:
    `HealthResponse::serving_state()` already distinguishes "degraded but
    serving" from "not serving" and was applied to the search-side gate under
    #4079. `SubprocessAnalyzeClient` now **consumes** `serving_state()` /
    `is_serving()` instead of re-deriving its own verdict, so one place decides
    what a trusty-search health payload means.
  - The gate is narrowed, not weakened: a genuinely not-serving trusty-search
    (embedder down, or a status that is neither `ok` nor `degraded`) still fails
    the probe as `Unavailable`. trusty-search's own status string is passed
    through verbatim rather than laundered into `"ok"`.
