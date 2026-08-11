Changed

- The MCP tool section of `README.md` is now generated from
  `tool_definitions()` by `tests/generated_docs.rs`. It replaces five
  hand-maintained category tables that between them listed 20 of the 45 tools,
  with a complete roster and a derived count. Regenerate with
  `UPDATE_DOCS=1 cargo test -p trusty-memory --test generated_docs` (#5205)
- `tool_definitions_lists_all_tools` no longer asserts a hardcoded tool count.
  The count is derived from the same function the README renders from, so the
  test now asserts its hardcoded roster and the served set are exactly equal in
  both directions instead (#5205)
