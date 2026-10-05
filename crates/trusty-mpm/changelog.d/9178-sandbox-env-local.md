Fixed
- `tm daemon --sandbox` admits `TRUSTY_SANDBOX` in its closed environment allowlist, and `scripts/sandbox_daemon.sh` passes `TRUSTY_SANDBOX=1`, so the sandboxed daemon loads no `.env.local` credentials (#9178).
