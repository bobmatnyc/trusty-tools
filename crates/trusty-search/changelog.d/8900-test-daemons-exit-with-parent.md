Fixed

- `start --foreground` now honours the `TRUSTY_EXIT_WITH_PARENT` parent-death stamp, and a daemon the CLI auto-starts inherits it, so a daemon a test starts exits when that test run ends instead of running on with ppid 1. Without the stamp, as under launchd or a hand run, nothing changes (Refs #8900).
