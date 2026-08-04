Added

- `panic_hook::install_panic_logger` — a process-wide panic hook that emits the
  panic payload, source location, thread name, and a force-captured backtrace
  as one `tracing::error!` before delegating to the previously installed hook.
  macOS `.ips` crash reports carry mangled Rust symbols but not the panic
  message, which left the literal cause of #4764's daemon aborts unrecoverable
  in production (#4764)
