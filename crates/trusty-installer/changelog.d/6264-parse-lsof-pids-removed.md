Breaking

- `commands::port_guard::parse_lsof_pids` is gone. The LISTEN-holder rewrite
  (#6264) replaced it with a parser that reads `lsof -FpT` per-file TCP state,
  so the old every-PID entry point has no caller and no meaning. Callers outside
  this crate must read the LISTEN holder through the guard's own entry point.
  This is what moves the version to 0.10.0 rather than 0.9.3.
