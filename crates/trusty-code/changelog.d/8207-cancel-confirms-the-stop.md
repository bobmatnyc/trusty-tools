Changed

- `session.cancel` now answers only once the cancelled task has actually stopped ([#8207](https://github.com/bobmatnyc/trusty-tools/issues/8207))
  - it used to return the still-`running` snapshot the instant the cooperative-cancel flag was set, so a client was told a task had stopped while it kept running and the next prompt was rejected with `-32003 invalid_argument` ("already has a task running")
  - the wait is fail-closed: a run that does not stop within 10s answers with a `-32010 cancel_unconfirmed` error, never a cancelled snapshot — a domain code a client can tell apart from a `-32603` daemon fault, and one that survives the socket transport, which carries no `data` field
  - `-32010` now has consumers: `POST /sessions/{id}/cancel` answers HTTP 503 ("not yet, retry") instead of 500, and the TUI client returns a typed `CancelOutcome::StillCancelling` carrying the daemon's own sentence. Rendering that as a "still cancelling…" line in the TUI is a separate change in `trusty-code-tui`
  - the 10s grace fits inside BOTH in-repo clients' 15s per-call budget — the TUI's and the CLI's; a compile-time assertion per client blocks any of them from being changed out of order, and the grace also bounds how long a cancel can stall a STDIO connection
  - a run that PANICKED now releases its execution slot and lands `Failed` with a `Failed` task result naming the crash, instead of leaving every later prompt rejected with `-32003` or resuming as if the user had cancelled it cleanly. A newer run that already holds the slot is never failed for its predecessor's crash
  - known limitation: the permission gate does not watch the cancel flag while a prompt is open, so Esc at an open permission prompt cannot stop the run until the prompt resolves — that cancel spends the full 10s grace and answers `-32010`
  - a cancel that times out no longer overwrites a newer run's live join handle with the old run's finished one
  - a second concurrent cancel on one session now waits for the first to confirm, instead of answering "tracked but not yet joinable"
  - a cancelled session is resumable: the next prompt runs on the SAME session, which is the one the TUI keeps for the whole conversation. `Failed` and `DeadlineExceeded` still require a fresh session
