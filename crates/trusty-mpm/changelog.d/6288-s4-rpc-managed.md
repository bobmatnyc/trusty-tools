Added

- The daemon's managed-session lifecycle, SESSCTL control-plane, and L2 proxy routes are now also served as JSON-RPC methods on the Unix socket (`mpm.managed.*`, `mpm.control.*`, `mpm.proxy.*`). HTTP is unchanged and every route still answers on it; each method and its HTTP route share one implementation, so the two transports cannot drift.
- The #6197 input validation and caller check moved into that shared implementation, so neither transport can reach a control-plane spawn without them. The socket's own peer-uid check is stricter than the loopback guard it stands in for.
- `mpm.control.connect` answers as a frame stream, carrying the same session events the SSE route's `data:` lines carry.
