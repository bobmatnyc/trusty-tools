Changed
- `tm doctor`'s `tcp_listeners` row fails on any trusty-search TCP listener instead of warning: the daemon binds no TCP port since #9214, so a live one is an older build or an opt-in `serve --with-http`. The `scripts/check_no_tcp_listeners.sh` allowlist keeps only the `serve --with-http` and `bind_with_auto_port` source sites for it.
