Breaking
- `ReviewResult` and `Finding` are `#[non_exhaustive]`; build them with
  `ReviewResult::new` and `Finding::new`, since a struct literal outside the
  crate no longer compiles (#9310).
- `ReviewResult::verdict_status` is now `Option<VerdictStatus>`, a closed set
  serialized as a string: `parsed`, `parse_failed`, `no_reviewer_output`,
  `all_withheld`, `suppressed_reject`. Every finalized result carries one, so
  `run --json`, the MCP `review_pr` / `review_diff` envelope, and a stored
  review record always hold the key; a clean review reads `parsed` (#9310).
- The `no_verified_findings` status is removed, with
  `withheld_contract::verdict_status` and
  `withheld_contract::VERDICT_STATUS_NO_VERIFIED_FINDINGS`. An approving
  review whose findings were all withheld reads `all_withheld`; a stored
  record that holds `no_verified_findings` still deserializes, as
  `all_withheld` (#9310).
- `run --json` now exits 0 for two reviews that exited non-zero as UNKNOWN
  before: a review whose findings were all withheld and whose reviewer asked
  for changes (REQUEST_CHANGES / `suppressed_reject`), and an approving review
  whose withheld findings had raised its verdict (APPROVE / `all_withheld`).
  Two all-withheld cases, a rejection whose findings were all advisory and a
  map-reduce synthesis APPROVE graded D or F, exited 0 as APPROVE before and
  still exit 0, now as REQUEST_CHANGES. Gate on `verdict` / `verdict_status`, not the exit code
  alone (#9310).
