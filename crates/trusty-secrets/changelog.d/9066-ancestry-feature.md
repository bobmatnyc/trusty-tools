Added
- The `ancestry` feature: the process-ancestry walk (`ancestry::has_agent_ancestor`, `ProcessTable`, `OsProcessTable`, `StartTime`) with std, `thiserror` and libc only, so trusty-console judges its HTTP client with the same walk the socket server uses and links no store, keychain, server or trusty-common. `server` includes it, and `server::ancestry` still resolves to it (#9066).
