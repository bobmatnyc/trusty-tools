Changed

- `fetch_managed_session_until_stopped` (bare-`tm` in-place relaunch) takes its
  retry budget as a parameter instead of reading `FETCH_RETRY_BUDGET` directly.
  The one production call site passes the same 400ms, so the in-place relaunch
  path behaves exactly as before; the parameter exists so the retry tests can
  drive a budget no loopback round trip can consume, which is what made
  `fetch_until_stopped_gives_up_after_budget_when_never_stopped` flaky under CI
  load (#5840).
