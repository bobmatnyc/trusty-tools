Changed
- The console refuses a memory daemon older than 0.31.0 (no `memory.protocol`) with a 502 naming the restart, and sends it nothing; it was called with a once-per-process warning before (#9537).
