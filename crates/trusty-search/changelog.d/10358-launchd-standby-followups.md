Fixed

- A start that is refused or stands by because another daemon holds the lock no longer rewrites that daemon's `daemon.env`, and a SIGTERM that arrives between a standby takeover and the daemon listening for it now drains and flushes instead of being swallowed.
