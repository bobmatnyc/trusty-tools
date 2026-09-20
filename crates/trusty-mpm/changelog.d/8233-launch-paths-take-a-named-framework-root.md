Fixed
- The managed launch no longer resolves its framework root, managed
  `CLAUDE_CONFIG_DIR`, session-MCP file or launch-spec directory from the process
  home. `ClaudeCodeAdapter` takes the root `DaemonState` already holds, so a test
  driving the real spawn or resume route can no longer redeploy the bundled agent
  and skill catalog into the operator's live `~/.trusty-mpm/framework/`. (#8233)
