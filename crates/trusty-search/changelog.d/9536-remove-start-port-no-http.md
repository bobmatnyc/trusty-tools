Breaking
- `trusty-search start` no longer accepts `--port` or `--no-http`. Both were ignored since the socket-only daemon (#9214); clap now rejects either as an unknown argument and exits 2. A launchd or systemd unit that still passes one fails to start: run `trusty-search service install` to regenerate a launchd unit, or delete the flag from a systemd `ExecStart=` line (#9536).
- `TRUSTY_SEARCH_NO_HTTP` is no longer read, and the deprecation warnings for all three inputs are gone (#9536).
