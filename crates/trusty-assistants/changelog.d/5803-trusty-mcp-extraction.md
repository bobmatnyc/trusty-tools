Changed

- **MCP protocol primitives now come from the `trusty-mcp` crate instead of `trusty_common::mcp`** — imports move from `trusty_common::mcp::…` to `trusty_mcp::…`, and the `trusty-common/mcp` feature is replaced by a direct `trusty-mcp` dependency. No behaviour change: the types and functions are byte-identical, only their home crate moved (ADR-0040, [#5803](https://github.com/bobmatnyc/trusty-tools/issues/5803))
