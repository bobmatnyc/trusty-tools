Added
- `trusty-search start --socket <PATH>` binds the daemon's RPC socket at exactly that path (#9214). The path must be absolute; a relative one is refused before the daemon starts. Without the flag the daemon binds `<TRUSTY_DATA_DIR>/trusty-search.sock`, or the shared default socket, as before. The HTTP listener is unchanged.
- `start --socket` creates a missing parent directory at `0700` and never changes the mode of an existing one other than the data directory: such a parent not already at `0700` is refused, because clients refuse a socket outside a `0700` directory (#9214).
