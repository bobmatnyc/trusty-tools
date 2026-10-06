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
- `run --json` now exits 0 for a review whose findings were all withheld and
  whose reviewer asked for changes (`suppressed_reject`, REQUEST_CHANGES); it
  exited non-zero as UNKNOWN before. Gate on `verdict` / `verdict_status`, not
  the exit code alone (#9310).
