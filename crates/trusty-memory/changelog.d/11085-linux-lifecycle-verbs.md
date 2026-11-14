Added
- On Linux, `trusty-memory start`, `stop` and a new `restart` run on the shared Linux lifecycle (#11085): `stop` stops only the daemon serving this data dir's socket and fails while a process survives SIGKILL; `restart` stops, confirms the stop, then starts the installed binary, and starts a daemon that is not running. macOS is unchanged.
