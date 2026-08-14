Changed

- Daemon-address resolution now runs through
  `trusty_common::daemon_guard::DaemonAddrLayout::TRUSTY_SEARCH`
  ([#5670](https://github.com/bobmatnyc/trusty-tools/issues/5670)). The private
  copy in `commands::daemon_utils` is deleted: `daemon_base_url()`,
  `daemon_port_path()`, `service::http_addr_path()`, and
  `service::write_http_addr_file()` are now thin bindings to the shared
  implementation, and `service::DEFAULT_PORT` reads its value from the same
  layout. Every discovery path, fallback, probe timeout, and refresh write is
  byte-for-byte what it was — the change exists so `tga`, which cannot depend on
  this crate, stops needing a second copy of the rules that #117 and #3545 were
  fixed in.
