Fixed

- **The log-drain scheduler's local idempotency-cache directory is no longer
  hardcoded inside the tick (#6537).** It is now a parameter supplied once by
  the daemon's production entry point (`~/.trusty-agents/log-drain`); the
  resolver, plan, and scheduler tick themselves moved into
  `trusty_common::log_drain` so trusty-agents and trusty-code share one
  implementation instead of two near-identical copies.
