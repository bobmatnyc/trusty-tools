Breaking
- `tctl` probes trusty-search over its Unix socket only, because the daemon binds no TCP port (#9214). A recorded `http_addr` or `:7878` is never dialled, so whatever answers there is not reported as trusty-search. `commands::probe_http::dual_transport` is removed, and `fixed_port_for("trusty-search")` is `None`.
- `tctl port` exits 1 for a member that serves a Unix socket (trusty-search, trusty-memory, trusty-analyze) and names the socket instead of printing a stale port. New: `commands::port::socket_only_refusal`.
