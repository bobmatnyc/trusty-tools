Fixed

- `page_find` with only `role`, only `name`, or both no longer answers `InvalidRequest`; the broker refused the null `text` the MCP server sends for an unused term. A refused `page_find` now names the argument in `field` (Refs #11860).
