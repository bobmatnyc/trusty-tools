Fixed

- The daemon no longer aborts from its disk-size metrics ticker. The shared
  `trusty_common::sys_metrics::dir_size_bytes` walk could raise a non-unwinding
  panic out of a directory-handle destructor, killing the process with no
  graceful shutdown — 40 self-aborts (`SIGABRT`) in one week, roughly every
  7 minutes under load, each relaunching via launchd `KeepAlive` into the same
  full auto-discover sweep that recreated the condition. The walk is now
  panic-safe; see the `trusty-common` entry for the mechanism (#4764)
- `trusty-search` now installs a panic hook at startup that logs the panic
  payload, location, thread, and backtrace through `tracing` before the default
  hook runs. macOS `.ips` crash reports do not carry the panic message, so
  daemon aborts previously reached the operator with the one datum that names
  the cause missing (#4764)
