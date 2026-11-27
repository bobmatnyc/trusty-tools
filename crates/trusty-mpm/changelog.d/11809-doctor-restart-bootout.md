Fixed

- `tm doctor`, the `tm start` refusal and the `tm daemon` refusal now suggest the release-workflow.md restart for a launchd-owned daemon: `launchctl bootout`, a wait of up to 30 s until `launchctl print` no longer finds the job, then one `launchctl bootstrap` of the registered plist. They no longer suggest `launchctl kickstart -k`, which SIGKILLs the daemon and skips request draining, and a bootstrap no longer races the exiting process into error 5. The MCP bridge's no-spawn hint offers a plain `kickstart` for a loaded-but-stopped job, never `-k` (#11809).
