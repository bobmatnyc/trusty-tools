Breaking
- The crate is renamed from `trusty-agents` to `trusty-assistants` (version restarts at 0.1.0) and the library from `trusty_agents` to `trusty_assistants` (#11632).
- The binary is now `ta`. `tagent` stays for one release as an alias that prints a deprecation line on stderr and then behaves exactly like `ta` (#11632).
- `--version`, the MCP `serverInfo` name, every MCP `clientInfo` name, the OpenRPC title and the log-drain object prefix now say `trusty-assistants` (#11632).
- `TA_ASSISTANTS_DIR` overrides the assistant homes root; `TAGENT_ASSISTANTS_DIR` is still honoured after it. Data stays where it was: homes in `~/trusty-agents/<id>/`, config and per-user state in `~/.trusty-agents/` (#11632).
