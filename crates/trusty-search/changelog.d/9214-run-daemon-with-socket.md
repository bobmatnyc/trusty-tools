Breaking
- `trusty_search::service::run_daemon_with` takes a third argument, `rpc_socket: Option<PathBuf>`, the socket to bind in place of `service::socket::socket_path()` (#9214). Pass `None` for the previous behaviour.
