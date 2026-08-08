Added

- `docgen` feature (off by default, test-facing): marker-delimited generated
  documentation regions. Renders MCP tool tables and counts from a crate's real
  descriptor function into `<!-- BEGIN GENERATED: <id> -->` regions in markdown,
  then asserts the checked-in copy matches — or rewrites it under
  `UPDATE_DOCS=1`. Rows sort by tool name so no map or source ordering reaches a
  committed file, and the `descriptor_source!` macro makes the cited symbol a
  compile-time reference rather than a hand-typed string. Adds no dependency
  (#5205)
