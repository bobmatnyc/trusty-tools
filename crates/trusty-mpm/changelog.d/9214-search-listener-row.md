Changed
- `tm doctor`'s `tcp_listeners` row cites #9214 instead of #6285 when it warns about a trusty-search TCP listener. It still warns rather than fails, because a trusty-search older than 0.59.0 binds 7878. The `scripts/check_no_tcp_listeners.sh` allowlist keeps only the `serve --with-http` and `bind_with_auto_port` source sites for trusty-search.
- The `tcp_listeners` doctor-row description says the trusty-search daemon binds no TCP port from 0.59.0 (#9214).
