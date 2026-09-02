Changed

- `LaunchdConfig::guard_short_grace` (the #6590 short-`ExitTimeOut` quiesce) is
  crate-visible instead of private to `launchd_activate`. `restart_gracefully`
  needs the same guard for the same reason — its bootout is bounded by the same
  loaded `ExitTimeOut` — and calls this one rather than growing a second copy.
