Fixed

- `tm doctor` and the other daemon-restart hints now suggest `launchctl bootout` then `launchctl bootstrap` for a launchd-owned daemon instead of `launchctl kickstart -k`, which SIGKILLs the daemon and skips request draining (#11809).
