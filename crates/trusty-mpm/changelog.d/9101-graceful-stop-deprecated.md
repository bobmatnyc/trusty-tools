Changed
- `ManagedTmuxDriver::graceful_stop` is deprecated. It kills by session name,
  and no production path calls it; prove ownership with `runtime_ownership`
  and kill with `kill_session_id` instead (#9101).
