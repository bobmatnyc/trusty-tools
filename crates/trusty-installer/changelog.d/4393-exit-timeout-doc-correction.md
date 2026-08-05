Documentation

- The `kickstart -k` hazard notes on `probe_http` and `verify_tail::needs_kickstart`
  described the shared plist renderer as emitting no `ExitTimeOut` and launchd
  as SIGKILLing 20 s after SIGTERM. The renderer now declares the key (#4393),
  and the pre-fix default measured 5 s, not 20 s. No behaviour change — the
  confirmed-down gate is unaffected (#4393)
