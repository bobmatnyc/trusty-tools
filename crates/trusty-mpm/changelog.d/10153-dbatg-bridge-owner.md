Fixed
- `tm serve --stdio` now refuses a `dbatg` MCP write to another PM session's Ledger store, or to another project's, before it reaches the daemon (LDG-3.6). A bridge that cannot determine its own session or launch directory refuses every `dbatg` write. Reads and own-session writes are unchanged.
