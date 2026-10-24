Breaking
- The `mcp::OrchestratorBackend` trait gains two required methods, `ask_owner` (#9856) and `sessions_chat` (#9979). Every out-of-tree implementation must add both; they have no default body.
