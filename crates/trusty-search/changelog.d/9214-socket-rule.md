Changed
- The daemon resolves its own socket through trusty-common's `search_rpc::search_socket_under`, the rule every client applies, so a client that exports the daemon's `TRUSTY_DATA_DIR` dials the same socket. The daemon's socket path is unchanged (#9214).
