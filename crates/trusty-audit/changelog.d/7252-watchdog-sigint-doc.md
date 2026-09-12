Documentation

- `stop_clones_on_interrupt`'s doc comment now states who installs the
  `SIGINT` handler and what happens when a host binary also awaits its own
  `tokio::signal::ctrl_c()` — both listeners are notified, since tokio fans
  the signal out rather than claiming it exclusively.
