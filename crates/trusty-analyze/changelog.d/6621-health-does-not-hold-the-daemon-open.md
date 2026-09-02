Fixed
- `analyze.health` no longer restarts the idle window. Every caller of it is a
  monitor — the console connector, the console's `console_metrics` MCP poll and
  `tctl`'s probe — each dialling every 15 s against a 600 s window, which kept
  one `trusty-analyze serve` process resident for 46 hours. It is registered as
  a liveness method now, so answering it costs the daemon nothing (#6621).
