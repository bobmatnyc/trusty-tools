Breaking
- `monitor::search_client` speaks the trusty-search daemon socket only (#9214). `resolve_search_url`, `normalize_url`, `DEFAULT_SEARCH_URL`, `SearchClient::base_url` and `SearchClient::set_base_url` are removed; use `resolve_search_socket`, `SearchClient::resolve` and `SearchClient::socket`. `SearchClient::new` takes the socket path, and `SearchClient::logs_tail` returns a `Result` instead of an empty list when the daemon does not answer.
- `monitor::search_tui::run_with_url` is renamed `run_with_socket` and takes the socket path (#9214).
