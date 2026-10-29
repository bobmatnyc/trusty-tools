Changed
- BREAKING: `memory_rpc` refuses a daemon with no `memory.protocol` (trusty-memory older than 0.31.0). `MemoryProtocol::PreHandshake`, the once-per-process warning and its extra `memory.health` probe are removed; a `-32601` answer is the new `MemoryProtocolError::NoHandshake`, which says to restart the daemon so it runs the installed release (#9537).
