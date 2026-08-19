Fixed
- The embedder supervision loop no longer busy-spins when the sidecar
  client's `unhealthy_signal` sender drops without ever firing. A closed
  `watch` channel makes `changed()` resolve `Ready(Err(_))` on every poll,
  and the old `Err` arm logged and looped back around — re-arming the same
  immediately-ready future and monopolising a tokio worker for the rest of
  the child's life (88,897 loop passes in 300ms, measured). The branch is
  now excluded from the `select!` while the channel is closed, so the loop
  keeps supervising through `child.wait()` and `shutdown_rx` alone. Unlike
  the sibling `shutdown_rx` fix (#3023), the disabled state is recomputed
  per iteration rather than latched, because a successful respawn replaces
  the receiver and the new sidecar's health must be watched again (#3026).
