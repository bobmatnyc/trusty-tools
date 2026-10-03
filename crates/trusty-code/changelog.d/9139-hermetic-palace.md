Fixed

- A test process no longer records turns into the operator's live memory
  palace. Under the test harness, the turn recorder and the PM catch-up read
  dial only an explicit `TRUSTY_MEMORY_SOCKET` under a system temp root; any
  other socket falls back to an unreachable placeholder (#9139).
- The e2e helpers spawn `tcode` with a cleared environment plus an allowlist,
  and give each child its own `HOME`, data root and memory socket, so an
  inherited `TRUSTY_MEMORY_PALACE` or live socket never reaches it (#9139).
