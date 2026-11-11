Fixed

- The Architect's alert-triage Haiku call (`scripts/alert_triage.py`, seeded by `tm fleet init`) now passes `--mcp-config={"mcpServers":{}}` beside `--tools ""` and `--strict-mcp-config`. `--tools ""` turns off ToolSearch deferral, so a call that loads user-scope MCP servers sends every server's tool schemas inline (about 383k first-request tokens measured). The empty config in `=` form loads no server, and the variadic `--mcp-config` cannot take a later argument as a config path (#11026).
