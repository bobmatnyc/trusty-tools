Added

- `RpcRouter::mark_cancel_on_hangup` marks a cancel-safe method that has side
  effects; the socket server drops its handler when the client hangs up, as it
  does for a `mark_read_only` method. `ClientGone` now reads "client hung up;
  cancelled method <name>" for either marker (Refs #12019).
