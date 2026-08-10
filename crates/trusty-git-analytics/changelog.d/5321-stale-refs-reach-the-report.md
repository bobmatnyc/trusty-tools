Fixed

- A repository `tga audit` collected from stale local refs is now named in the
  report's Gaps & Caveats section, with its remote and the fetch error, and
  states that its data may be behind the true remote state. The sweep hard-codes
  `--allow-stale`, so an unreachable remote leaves the `collect` stage reporting
  `ok`; the per-repository fetch outcomes were printed to stderr and dropped, and
  a report over months-old refs was indistinguishable from a current one (#5321,
  DOC-67 §9). `commands::collect::run_reporting_fetch` is the new entry point
  that returns those outcomes — `run` and `run_with_progress` are unchanged and
  still return `()`. The sweep's terminal table qualifies the same stage as
  `ok (N stale)`; a run where every remote was reached renders as before.
