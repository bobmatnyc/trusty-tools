Changed

- `session.cancel` now answers only once the cancelled task has actually stopped ([#8207](https://github.com/bobmatnyc/trusty-tools/issues/8207))
  - it used to return the still-`running` snapshot the instant the cooperative-cancel flag was set, so a client was told a task had stopped while it kept running and the next prompt was rejected with `-32003 invalid_argument` ("already has a task running")
  - the wait is fail-closed: a run that does not stop within 10s answers with a `-32010 cancel_unconfirmed` error, never a cancelled snapshot — a domain code a client can tell apart from a `-32603` daemon fault, and one that survives the socket transport, which carries no `data` field
  - the 10s grace fits inside the TUI's 15s per-call budget; a compile-time assertion blocks the two from being changed out of order, and it also bounds how long a cancel can stall a STDIO connection
  - a run that PANICKED now releases its execution slot instead of leaving every later prompt on that session rejected with `-32003`
  - a cancel that times out no longer overwrites a newer run's live join handle with the old run's finished one
  - a second concurrent cancel on one session now waits for the first to confirm, instead of answering "tracked but not yet joinable"
  - a cancelled session is resumable: the next prompt runs on the SAME session, which is the one the TUI keeps for the whole conversation. `Failed` and `DeadlineExceeded` still require a fresh session
