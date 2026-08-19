Changed

- `serve` names the interface it binds as `LOOPBACK_BIND` instead of an unnamed
  `[127, 0, 0, 1]` literal (#6038). Behaviour is identical — the daemon answers
  on the IPv4 loopback and only there (ADR-0018) — but a client whose default
  URL said `localhost` looked correct while resolving `::1` first on macOS, and
  nothing in this file stated the address a client has to match.
