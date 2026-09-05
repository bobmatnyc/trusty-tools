Added

- `review_gap_contract`: the one definition of the "trusty-analyze lane DID NOT
  RUN" headline a `trusty-review` report leads a dead analyze lane with, plus the
  `analyze_lane_is_dead` predicate its readers apply. `trusty-review` and
  `trusty-audit` deliberately do not depend on each other (DOC-67 §5), so each
  spelled the phrase as its own literal and they drifted (#6784).
