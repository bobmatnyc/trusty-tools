Breaking
- `TrustySearchClient::new` takes the trusty-search socket path instead of an HTTP base URL; `TrustySearchClient::from_env` builds one at the standard path (#9214).
- `TrustySearchClient::base_url()` is removed; `socket_path()` names the socket the client calls (#9214).
- The `--search-url` flag and the `TRUSTY_SEARCH_URL` environment variable are removed. trusty-analyze reaches trusty-search only over its Unix socket, with no TCP fallback (#9214).
