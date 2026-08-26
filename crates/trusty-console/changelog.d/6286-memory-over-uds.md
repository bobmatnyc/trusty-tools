Changed
- `MemoryConnector` dials `memory.health` on trusty-memory's Unix socket instead of reading `~/.trusty-memory/http_addr` and probing the port it named (#6286, ADR-0032). Nothing rewrites that dotfile any more, so a connector still reading it would report health from a permanently stale address
