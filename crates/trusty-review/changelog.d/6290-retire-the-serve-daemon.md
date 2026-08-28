Removed

- The `serve` daemon, the `socket` subcommand and the `service` launchd wrapper
  are gone — trusty-review runs per invocation, with no resident process and no
  LaunchAgent (#6290, ADR-0032).
- `trusty-review run --json` prints the same `ReviewResult` object the retired
  `review.run` method returned, field for field.
- A failed run now prints that JSON with the reason in `error` AND exits
  non-zero; it previously exited 0 on a provider outage.
- The MCP stdio service moved to `trusty-review mcp`. `serve --stdio` is kept as
  an alias, so no existing `.mcp.json` needs editing.
- The `service::rpc` module is gone from the library: `serve`,
  `serve_with_shutdown`, `build_router`, `socket_path`, the `METHOD_*` and
  `MAX_FRAME_BYTES` constants, and `NoParams` are all removed. This is the
  break behind the 0.29.0 bump.
