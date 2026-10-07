Added
- Every finding in a finished review, and every withheld finding, carries a
  `severity` of `low`, `medium`, `high` or `critical` in `run --json`, the
  MCP `review_pr` / `review_diff` envelope and the stored review record. A
  severity the reviewer gave is kept, so `critical` stays `critical`, but it
  never exceeds what the finding's final effort allows: a `critical` finding
  a gate demoted to Medium effort reads `medium`, and a High finding with no
  citation and no `code_provable` flag reads at most `medium`. With no usable
  reviewer severity it is derived from effort and is never `critical`.
  Severity changes no verdict and no grade (#9310).
