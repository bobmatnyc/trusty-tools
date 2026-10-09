Breaking
- `tctl ensure` and `tctl ensure --wait` reach trusty-search over its Unix socket only. A stale `http_addr` file is never read and no TCP connection is made (#9214).
- `commands::ensure::project_setup::register_index` takes the trusty-search socket path instead of a `reqwest::Client`, and `commands::ensure::readiness::probe_ready` takes no arguments.
- Removed `commands::ensure::daemon::{resolve_base_url, build_client, health_ok}`. Use `search_socket`, `search_serving` and `search_healthy` in the same module instead.
