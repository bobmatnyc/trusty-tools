Performance

- trusty-mpm builds one full binary, `tm`. The `trusty-mpm` binary is now a
  small alias that execs the `tm` installed beside it, with the same
  arguments, stdin and exit status, instead of a second compile and link of
  the whole CLI. `cargo install trusty-mpm` still installs both names, so
  launchd plists, hook commands and `.mcp.json` entries that name
  `trusty-mpm` keep working. A daemon started through the alias now runs as
  `tm`, and the hooks it rewrites name `tm`.
