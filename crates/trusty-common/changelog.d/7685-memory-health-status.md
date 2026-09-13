Added

- `memory_rpc::MemoryHealthStatus` reads the `status` of a trusty-memory `memory.health` answer (`Ok`, `Degraded`, `Wedged`, `Unrecognised`, `Missing`), so consumers share one client-side parser and only `ok` reads as healthy. (#7685)
