Breaking
- `review_body` no longer carries the reviewer's summary prose or its fenced
  review JSON on any path, map-reduce included. A caller that re-parsed the
  body for an embedded `grade` or `verdict` reads the top-level `grade` and
  `verdict` instead. An aborted review's body was empty, or the truncated
  reply; it is now the summary sentence for `no_reviewer_output` (#9310).
- New public field `Finding::severity: Option<Severity>` and new public enum
  `models::Severity` (`low`, `medium`, `high`, `critical`). `Finding` is
  `#[non_exhaustive]`, so no struct literal breaks, but a finished review's
  serialized findings and withheld findings gain a `severity` key. A record
  without the key still deserializes, with `severity` as `None` (#9310).
