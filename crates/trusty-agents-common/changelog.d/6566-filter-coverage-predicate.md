Added

- `compress::has_filter_for(tool_name)` reports whether any native filter
  covers a tool name, so a caller upstream of the dispatch can skip work that
  would return its input unchanged. It is backed by `compress::classify_tool`,
  which returns the new `ToolFilter` enum; `compress_tool_output` now routes
  through that same classification with an exhaustive match, so a filter
  cannot be added to the dispatch without the predicate seeing it.
