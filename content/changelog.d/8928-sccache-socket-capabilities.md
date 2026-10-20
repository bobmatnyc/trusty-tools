Added
- The `tm-capabilities` doctor reference describes the new `sccache_socket` row: whether the sccache unix-socket server for unsandboxed cargo builds answers on `~/.trusty-mpm/sccache.sock` when `build.sccache` is true, and why sandboxed sessions never use it (#8928 step 3).
