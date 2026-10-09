Added
- `trusty-search start --socket <PATH>` binds the daemon's RPC socket at exactly that path (#9214). The path must be absolute; a relative one is refused before the daemon starts. Without the flag the daemon binds `<TRUSTY_DATA_DIR>/trusty-search.sock`, or the shared default socket, as before. The HTTP listener is unchanged.
