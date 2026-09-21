Added
- `DaemonBridgeJsonRpc::with_local_handler` answers chosen MCP methods in the bridge process instead of forwarding them, so a daemon that is down at handshake no longer makes an MCP client mark the server failed for the whole session (#8351).
- `DaemonBridgeJsonRpc::with_socket_resolver` re-resolves the daemon's socket for every forwarded request, so a bridge that resolved a stale or wrong path heals on the next call rather than staying broken for the life of the process. A resolver that fails is reported as an error carrying the request's id, never downgraded to the configured path (#8351).
- `UdsBridgeConfig::with_bridge_version` names the consumer's build in transport-error text, so the next report of an unreachable daemon is attributable to a binary (#8351).
