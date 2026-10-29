Fixed
- `run_bounded` children no longer inherit tm's open sockets on macOS, so a probe spawned while a connection is being created cannot keep that connection open after tm drops it (#10083).
