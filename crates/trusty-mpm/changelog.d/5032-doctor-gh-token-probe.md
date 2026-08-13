Fixed

- `tm doctor`'s `gh_account` check no longer reports "gh is not authenticated"
  for a working `GH_TOKEN` / `GITHUB_TOKEN` login. Its `gh auth status` probe
  was bounded at 250 ms — a constant inherited from the statusline's render
  path — while validating an env token takes a network round trip that measured
  281–389 ms, so every probe timed out and the timeout rendered as a definite
  negative. The doctor now probes under its own 5 s bound, and a probe that
  still does not finish reports the auth state as UNKNOWN with the reason,
  instead of claiming the account does not exist.
- The doctor reads the active login and the logged-in list from ONE
  `gh auth status` invocation (previously two, one of them `--active`), so the
  check costs a single network round trip.
