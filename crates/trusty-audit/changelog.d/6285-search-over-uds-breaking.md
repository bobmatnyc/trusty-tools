Breaking
- `grounding::Tools::search_url: String` is now `grounding::Tools::search_socket: PathBuf` — the trusty-search half of the pair names a Unix socket rather than a base URL.
- `grounding::evidence::HttpSearch` is now `grounding::evidence::SocketSearch`, and its constructor takes a socket path and is infallible: there is no HTTP client left to fail to build.
- `grounding::daemons::search_base_url` is removed. `grounding::daemons::search_socket` replaces it, forwarding to the new `grounding::search_rpc::search_socket`.
