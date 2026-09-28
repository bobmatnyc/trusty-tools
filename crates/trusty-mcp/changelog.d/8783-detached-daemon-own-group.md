Fixed
- The daemon `ensure_daemon_up` and `ensure_daemon_up_single_flight` auto-start now starts in its own session (`setsid`, Unix), so a group kill or Ctrl-C aimed at the stdio bridge no longer kills it. This is the auto-start path of `trusty-search serve` and of `tm`'s MCP stdio bridge (#8783).
