Added

- `uds::server::RpcRouter::fallback` mounts a service's own generic
  `(method, params)` dispatcher as the router's catch-all, so a daemon with an
  existing dispatch table serves it whole instead of re-registering every method
  by name (#6286). The fallback is consulted only after the registered-method
  lookup misses, so a name registered with `RpcRouter::method` still wins, and an
  error it returns crosses the wire as a JSON-RPC error frame with its own code
  and message intact. A router that sets no fallback is unchanged: an unknown
  method still answers method-not-found naming the methods it does serve.
