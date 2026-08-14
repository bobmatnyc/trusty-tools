Added

- `daemon_guard::DaemonAddrLayout` resolves a daemon's base URL from its
  on-disk discovery files
  ([#5670](https://github.com/bobmatnyc/trusty-tools/issues/5670)). The logic
  was private to trusty-search's CLI, so a crate that has to probe that daemon
  without depending on it — `tga` — had no way to ask. `resolve_base_url()`
  prefers the `host:port` address file, proving it reachable over TCP first
  (#117), falls back to the port-only file, and finally to the layout's default
  port; the address file is refreshed when the fallback address answers.
  `DaemonAddrLayout::TRUSTY_SEARCH` is the shipped layout, and
  `discovery_file_path()` / `port_file_path()` expose the two paths on their
  own. Behaviour is unchanged from the trusty-search original, including the
  asymmetric fallback directories: with the isolation env var unset the address
  file sits under `$HOME` and the port file under the platform data-local dir.
- `daemon_guard::write_addr_file_atomic` writes a discovery line via
  tmp-file + fsync + rename, so a reader never observes a torn file. It is the
  one writer behind both the daemon's own write and the resolver's stale-file
  refresh.
