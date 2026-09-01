Added

- The header lockup names the running console version — `UNIT-05 · SERVICE CONSOLE · v0.9.2`. The version is read from the server's existing `GET /health` on mount rather than compiled into the SPA bundle, which is committed and would otherwise go stale; until that probe answers, the descriptor renders unchanged.
