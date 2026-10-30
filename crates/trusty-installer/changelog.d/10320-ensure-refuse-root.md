Security
- `tctl ensure` refuses to run as root (effective uid 0) and exits 1 before it writes `.mcp.json`, its `.bak` or a temp file, or dials a daemon socket. The error says not to use sudo and gives `sudo chown $USER <file>` for a file that is already root-owned (#10320).
- `tctl install` still runs as root, but its verify tail now skips the project `.mcp.json` patch and the search-index / memory-palace setup, and prints a warning with the same `sudo chown $USER <file>` hint. The install exit code is unchanged by the skip (#10320).
