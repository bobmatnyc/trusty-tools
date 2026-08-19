Changed

- **MCP protocol primitives now come from the `trusty-mcp` crate instead of `trusty_common::mcp`** — imports move from `trusty_common::mcp::…` to `trusty_mcp::…`, and the `trusty-common/mcp` feature is replaced by a direct `trusty-mcp` dependency. `trusty-common` stays a dependency for `init_tracing`. No behaviour change (ADR-0040, [#5803](https://github.com/bobmatnyc/trusty-tools/issues/5803))
