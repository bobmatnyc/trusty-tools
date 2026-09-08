Fixed

- **`~/.trusty-code/logs` and its rolled log files are now private (#6537).**
  `init_tracing_with_file_log` routes the directory through the crate's
  private-state hardening (`0700`) instead of a raw `create_dir_all`, and
  each rolled log file is chmod'd `0600` on creation.
- **The log-drain scheduler's local idempotency-cache directory is no longer
  hardcoded inside the tick (#6537).** It is now a parameter supplied once by
  the daemon's production entry point (`~/.trusty-code/log-drain`); the
  resolver, plan, and scheduler tick themselves moved into
  `trusty_common::log_drain` so trusty-code and trusty-agents share one
  implementation instead of two near-identical copies.
