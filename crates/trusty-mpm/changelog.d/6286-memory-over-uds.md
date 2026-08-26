Changed
- Every trusty-memory call goes over the daemon's Unix socket (#6286, ADR-0032). `tm doctor`'s memory check, the TUI health panel, the startup banner's palace assertion, `tm memory import`, catch-up and the identity seeder all dial the derived socket instead of reading `~/.trusty-memory/http_addr`
- `tm memory import --memory-url` is `--memory-socket`, and takes a path
- The banner creates the `user` palace only on a genuine not-found, read off the JSON-RPC error code where it used to be read off a 404 status
