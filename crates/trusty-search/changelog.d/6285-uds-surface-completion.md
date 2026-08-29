Added
- The Unix-socket RPC surface serves the last four routes with a named consumer:
  `search.index.config.set`, `search.config.set`, `search.logs.tail` and
  `search.registry.orphans`. Each runs the same core its HTTP route runs, so a
  caller moving onto the socket gets the same body and the same refusals (#6285).
