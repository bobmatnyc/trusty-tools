Added
- `StdioMcpClient::spawn_grouped_with_env` spawns an MCP server as the leader of its own process group (unix). Dropping or respawning the client sends `SIGKILL` to the whole group, so a wrapper command's grandchild (`npx`, `uvx`) does not outlive it. Existing spawn constructors are unchanged (#11650).
