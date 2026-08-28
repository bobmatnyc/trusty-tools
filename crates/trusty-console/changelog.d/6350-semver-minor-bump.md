Changed
- trusty-console moves to 0.8.0. `cargo-semver-checks` reports
  `inherent_method_missing` for `ReviewConnector::with_socket` against the
  published 0.7.0 baseline — removed by #6290 when the review daemon was retired
  and the connector moved to a presence check. For a `0.y.z` crate the breaking
  bump is the MINOR position, so 0.7.1 was never a legal position for it. The
  root workspace requirement moves from `0.7.0` to `0.8.0` (#6350).
