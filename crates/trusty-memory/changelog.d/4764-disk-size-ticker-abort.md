Fixed

- The daemon inherits the #4764 panic-safety fix for its disk-size metrics
  ticker, which calls the same shared `trusty_common::sys_metrics::dir_size_bytes`
  walk that self-aborted `trusty-search` 40 times in a week. `trusty-memory` had
  produced no crash reports of its own, but shared the vulnerable code path
  exactly (#4764)
- `trusty-memory` now installs a panic hook at startup that logs the panic
  payload, location, thread, and backtrace through `tracing` before the default
  hook runs, so a future abort arrives with its cause in the log stream rather
  than only as a symbol-mangled macOS `.ips` report (#4764)
