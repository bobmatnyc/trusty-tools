Added

- `missing_docs` JSON-RPC method: given a list of doc ids, returns the subset the
  daemon does not hold. Callers can now establish index coverage as a set
  statement instead of inferring it from `stats.doc_count`, which is satisfied by
  documents the caller never asked about.
- `run_until(config, shutdown)` runs the daemon against an explicit shutdown
  trigger. `run` supplies the real SIGTERM/SIGINT trigger and is otherwise
  unchanged; the split gives the shutdown snapshot flush a deterministic,
  CI-runnable regression test instead of a timing-dependent `#[ignore]`d one.
  Signal handlers are now installed before snapshot load and socket bind, so a
  SIGTERM during startup reaches the shutdown path.
