Removed
- The unused public `trusty_search::service::client::SearchClient` HTTP client (#9214). Use `trusty_search::service::daemon_client::DaemonClient`, which speaks the daemon socket.
