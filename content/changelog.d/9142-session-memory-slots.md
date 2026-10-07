Added
- `tm-session-management`, `tm-session-pause` and `tm-session-resume` tell the PM to record session resume state under `ws:<session>/resume` and PR state under `pr:<n>/state` with `tm memory remember --fact-key` (or the MCP tools' `fact_key`), so recall returns the current state instead of stale snapshots (#9142).
