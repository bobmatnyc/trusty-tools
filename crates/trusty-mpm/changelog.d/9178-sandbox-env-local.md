Fixed
- `tm daemon --sandbox` admits `TRUSTY_SANDBOX` in its closed environment allowlist, and `scripts/sandbox_daemon.sh` passes `TRUSTY_SANDBOX=1`, so the sandboxed daemon loads no `.env.local` credentials (#9178).
- `tm daemon --sandbox` now refuses to start unless `TRUSTY_SANDBOX` is exactly `1`; an absent, empty, `0`, `true` or non-UTF-8 value refuses with a message that names the variable and never its value, so a hand-built sandbox cannot load `.env.local` credentials (#9178).
