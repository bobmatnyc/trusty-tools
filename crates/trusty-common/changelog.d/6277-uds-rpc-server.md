Added

- `uds::server` serves the other end of `uds::rpc`: a caller registers method
  names against handlers over its own request and response types
  (`RpcRouter::typed`), and `RpcServer::run` binds through `bind_hardened`,
  checks the peer uid on every accepted connection, dispatches each in its own
  task, and unlinks the socket on shutdown (#6277). An unknown method answers a
  JSON-RPC method-not-found frame rather than dropping the connection, so a
  drifted client reads the reason instead of a transport failure. Existing
  callers are unaffected — `webhook_relay` keeps its own single-method listener.
