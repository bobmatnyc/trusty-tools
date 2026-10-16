Added
- New `sessions_chat` MCP tool sends one coordinator chat turn, the same turn as the daemon's `mpm.sessions.chat` method, and returns its `{ reply, conv_id, actions_taken, cost }` response unchanged. With no model configured it fails with a `sessions_chat failed (503): …` error text (#9979).
