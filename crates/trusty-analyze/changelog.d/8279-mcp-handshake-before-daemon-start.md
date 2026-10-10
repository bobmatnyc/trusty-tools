Fixed
- `trusty-analyze mcp` now answers `initialize` and `tools/list` at once instead of waiting up to 30 s for the daemon to start, so a loaded host no longer times out the console's handshake. The daemon auto-start runs once in the background; the first tool call waits for it, and an auto-start failure comes back as that call's error instead of exiting the bridge (#8279).
