Added
- The daemon's Unix socket now serves `search.index.quantize`, the twin of `POST /indexes/{id}/quantize`, in the same bulk admission lane (#6285).
- `trusty-search quantize` reaches the daemon over its Unix socket instead of the loopback HTTP port. When no daemon answers the socket, the command fails and names the socket path; it never falls back to TCP (#6285).
- `service::daemon_client::DaemonClient`, the socket client the CLI and MCP bridge move onto, reports refusals with the daemon's own code: not found, conflict, invalid params, unavailable, and permanently unavailable (#6285).
