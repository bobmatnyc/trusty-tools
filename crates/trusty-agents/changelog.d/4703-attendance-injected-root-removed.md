Removed

- `attendance::note_turn` — the `$HOME`-resolving wrapper behind that trap — is
  deleted rather than hidden behind `#[cfg(not(test))]`. A `cfg` gate would only
  close the trap for unit tests compiled into this crate; integration tests,
  doc-tests and downstream consumers all compile the library without
  `cfg(test)`, so the function would remain reachable for them. Deleting it
  removes the trap from every build configuration, and turns "I forgot to inject
  a root" from a silently-passing test into a compile error. `$HOME` resolution
  moved up to construction (`AppState::default`, `run_slack_bot`,
  `run_telegram_bot`), which stores it in a field a test can point at a tempdir.
