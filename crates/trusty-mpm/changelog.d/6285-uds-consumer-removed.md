Removed

- The `daemon::discover` module, `TrustyAddrs`, `TRUSTY_SEARCH_DEFAULT_ADDR` (the
  compiled-in `127.0.0.1:7878`), and `DaemonState::{set_trusty_addrs,
  trusty_addrs}`. trusty-search has no port to discover since ADR-0032, nothing
  read the stored addresses, and `~/.trusty-search/http_addr` is stale on every
  machine that ran the old daemon.
