Security
- The enrollment check refuses unsafe Chrome switches given with one dash (`-load-extension=...`, `-user-data-dir=...`) as well as two, including after a bare `--` separator; `--remote-debugging-address`, `--enable-automation` and `--remote-allow-origins` are now unsafe too
- The native host re-reads the broker's peer PID after its executable and argv checks, closing a PID-reuse window
- The argv[0] pin compares strings before it canonicalizes a peer-chosen path
- The enrollment `HostCheck` seam and `BrokerOptions::host_check` exist only under the `test-seams` feature, so a release build cannot install an open check
