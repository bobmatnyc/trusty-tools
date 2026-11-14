Changed
- `tagent --api` and `tagent --serve` exit non-zero under `TAGENT_NO_API=1`, before any listener binds or the Telegram gateway starts (#6433, DOC-90 AG-1).
- The API refuses any non-loopback `--bind`, with or without an API token, and no longer probes for a LAN address or prints a LAN URL (#6433, DOC-90 AG-2).
