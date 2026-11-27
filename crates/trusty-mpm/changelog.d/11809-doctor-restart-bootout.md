Fixed

- `tm doctor`, the `tm start` refusal and the `tm daemon` refusal now suggest `launchctl bootout` then `launchctl bootstrap` of the registered plist for a launchd-owned daemon instead of `launchctl kickstart -k`, which SIGKILLs the daemon and skips request draining. The MCP bridge's no-spawn hint offers a plain `kickstart` for a loaded-but-stopped job, never `-k` (#11809).
