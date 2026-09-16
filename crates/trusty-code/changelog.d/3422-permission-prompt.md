Added
- The TUI client now renders permission prompts (#3422): `permission_requested` and `permission_resolved` map to real TUI events instead of being dropped, and `CodeEngine::respond_permission` answers a suspended tool call over the daemon's own `session.permission.respond` RPC.
