Changed
- `protocol_mismatch` also reports a daemon with no `memory.protocol` (older than 0.31.0), so a probe shows it degraded with the restart hint instead of calling it (#9537).
