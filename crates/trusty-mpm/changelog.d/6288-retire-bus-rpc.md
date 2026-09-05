Removed

- The four peer-bus JSON-RPC methods — `mpm.bus.register`, `mpm.bus.deregister`, `mpm.bus.list` and `mpm.bus.publish` — are no longer served over the daemon's Unix socket, and a call to any of them answers `method_not_found`. trusty-console hosts the only event bus (owner ruling 2026-09-05), and mpm publishes to it through the `trusty-common` PushClient instead. `PeerBus` itself is unchanged and still reachable over HTTP (#6288).
