Fixed

- The run index's dead-analyze-lane count now keys on the shared
  `trusty_common::review_gap_contract` headline instead of a literal of its own,
  so it recognises every gap line `trusty-review` writes for a total collapse.
  It previously missed the client-build-failure path and undercounted
  `Rollup::analyze_lanes_dead()` (#6784).
