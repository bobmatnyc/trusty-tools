Fixed
- Action events still buffered when the daemon stops are no longer lost. The final flush sends what the trusty-events bus takes, writes the rest to `<data dir>/action-events-spool/`, and the next start sends them once. The stop stays bounded (1 s on a normal stop, 200 ms on SIGTERM); any event neither sent nor spooled is logged with a `lost` count (#10412).
