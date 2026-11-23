Fixed
- MCP servers are spawned in their own process group, so a wrapper command's grandchild (`npx`, `uvx`) is killed when the server's client is dropped instead of being left running (#11650).
