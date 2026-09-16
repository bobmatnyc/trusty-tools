Documentation

- `web-qa` now states never to close the last other tab in the MCP tab
  group mid-task, and to call `tabs_context_mcp` on the first unexpected
  tool error to check for a lost tab group before retrying
  (refs [#8022](https://github.com/bobmatnyc/trusty-tools/issues/8022))
