Fixed

- `--analyze` sizes each request to the work the endpoint actually does
  - `analyze.complexity_distribution` and `analyze.refactor_suggestions` grade
    every chunk of the index rather than reading computed data, which takes
    41-46 s on a 104k-chunk checkout against the 15 s they were allowed. Both
    now get a corpus budget defaulting to 180 s, so the §7 complexity table is
    filled instead of rendering `not stated in source data` on every large
    repository (#6712)
  - `--analyze-timeout-secs` and the manifest key `[report].analyze_timeout_secs`
    set that budget, in that order of precedence; it also raises the diagnostics
    budget when it exceeds that endpoint's own deadline ladder. The readiness
    probe keeps its 15 s, so an unreachable daemon is still declared unreachable
    in seconds (#6712)
  - A completed analyze request logs its elapsed time at INFO, and a request
    that runs out of time still names the budget it exhausted (#6712)
