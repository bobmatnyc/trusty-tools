Changed
- `query`, `doctor`, `monitor status` and `monitor indexes`, and the daemon's auto-discovery now reach the daemon over its Unix socket only (#9214). With no daemon answering they fail with an error naming the socket, never a guessed `127.0.0.1:7878`.
- `doctor` reports the HTTP listener the daemon says it bound (or "socket-only") in place of a TCP probe of the port file's port.
