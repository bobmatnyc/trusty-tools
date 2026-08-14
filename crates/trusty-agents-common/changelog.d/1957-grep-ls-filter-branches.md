Added

- `compress_tool_output` now compresses `grep`/`rg`/`find` match-or-path
  lists and `ls` directory listings, which previously passed through
  unchanged (0% reduction, per the #1953 spike). Long lists are head/tail
  capped with an explicit `... N lines omitted ...` marker rather than
  silently dropped.
