Changed

- The MCP tool section of `README.md` and `CLAUDE.md` is now generated from
  `mcp::tool_descriptors()` plus `mcp::descriptors::review_tool_descriptors()`
  by `tests/generated_docs.rs`. The feature-dependent surface is stated as
  derived numbers — 19 tools with default features, 22 with `--features
  review` — with a per-row `Available` column, replacing prose that told the
  reader to go read `tool_descriptors()` because no fixed number was safe.
  Regenerate with
  `UPDATE_DOCS=1 cargo test -p trusty-analyze --test generated_docs` (#5205)
- `review_tool_descriptors()` moved from the `#[cfg(feature = "review")]`
  `mcp::review` module to `mcp::descriptors`, so the three `tr_review_*`
  descriptors compile in every build. Dispatch stays feature-gated and
  `tools/list` is unchanged in both configurations; the move is what lets a
  default build — the only one CI runs — verify the documented review rows
  (#5205)
- `README.md` keeps its HTTP-equivalents table hand-written, because the route
  a tool forwards to is not in the descriptors. It now sits outside the
  generated markers and every tool name in it is asserted to be real by
  `http_equivalents_name_only_real_tools` (#5205)
