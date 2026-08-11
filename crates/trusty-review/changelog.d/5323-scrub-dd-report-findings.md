Security

- DD-report findings are now scrubbed of this process's credentials before they
  reach the report (#5323). trusty-analyze's daemon output, a manifest-declared
  metrics JSON, and the investigation's verified findings previously travelled
  verbatim into the rendered finding bands, the executive summary, the synthesis
  digest sent to the LLM provider, and the JSON twin — tga's report path has had
  this guarantee since #5239 and trusty-review's had none.
- An investigation finding reaches the page by two independent routes and both
  now scrub. `apply_investigation` covers the metrics route and the investigation
  record stored on the model; `merge_investigation_prose` covers the synthesis
  route, which `FindingRow::merge_prose` overwrites the metrics prose with. The
  verbatim `evidence_quote` has no metrics route at all and is only ever scrubbed
  on the second.
- The scrub runs where findings enter each sink, ahead of every downstream
  truncation, and covers every producer-supplied string — including
  `AnalyzeMetrics.schema_version`, which a declared metrics JSON authors freely.
  Needles come from `trusty_common::credentials`' registry walk, so no secret is
  passed across a process boundary.
