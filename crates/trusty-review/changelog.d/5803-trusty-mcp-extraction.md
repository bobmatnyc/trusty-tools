Changed

- **MCP protocol primitives now come from the `trusty-mcp` crate instead of `trusty_common::mcp`** — imports move from `trusty_common::mcp::…` to `trusty_mcp::…`, and this crate's own `mcp` feature forwards to `dep:trusty-mcp` rather than `trusty-common/mcp`. The feature is still on by default, so a default build is unaffected. No behaviour change (ADR-0040, [#5803](https://github.com/bobmatnyc/trusty-tools/issues/5803))
