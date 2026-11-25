Changed
- `tagent --api` and `tagent --serve` exit non-zero when `TAGENT_NO_API` is set, before any listener binds or the Telegram gateway starts. Any value except empty, `0`, `false`, `no` or `off` refuses (#6433, DOC-90 AG-1).
- The API refuses any non-loopback `--bind`, with or without an API token, and no longer probes for a LAN address or prints a LAN URL (#6433, DOC-90 AG-2).
- Drops the unused `local-ip-address` dependency (#6433).
