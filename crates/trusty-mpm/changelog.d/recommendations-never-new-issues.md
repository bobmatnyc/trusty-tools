Documentation

- `BASE-AGENT.md`, `self-improvement-loop`, and `tm-ticketing` now state that
  an agent's Improvement recommendations and self-improvement findings go to
  the `bobmatnyc/trusty-tools` rollup issue #8021, or as a comment on the
  parent issue, and never as a new issue (owner ruling 2026-09-27). The
  `tm-ticketing` skill and `TICKETING.md` also state that a sweep closure
  (age, staleness, duplicate, or obsolete) carries the `closed:sweep` label,
  which a fix closure from a merged PR never carries.
- `rust-delivery-workflow` now states that a `--include-ignored` gate run
  excludes every profiling/benchmark test binary, run at most one at a time
  and only for a performance-touching change, naming the excluded targets in
  the report (owner ruling 2026-09-27).
