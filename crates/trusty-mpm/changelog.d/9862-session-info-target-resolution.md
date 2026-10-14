Fixed
- `tm sessions info` now resolves a unique name prefix, not only an exact id or name, across project and managed sessions in one lookup. A prefix that matches more than one session, in either list, is an error naming every match (#9862).
- `tm sessions info <project-session-id>` now finds the project session. Before, only the tmux name matched a project session (#9862).
- When the managed session list cannot be read, `tm sessions info` reports `could not list managed sessions to resolve '<target>'` with the cause, instead of "not found". A daemon with no managed-session route (HTTP 404) gets the same error, saying the daemon is older than the CLI (#9862).
