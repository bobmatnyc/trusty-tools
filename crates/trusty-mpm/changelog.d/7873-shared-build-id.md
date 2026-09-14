Fixed

- `tm doctor`'s `daemon_version` check now compares a compile-time build id emitted once per package build by `build.rs`, so the `tm` bin running the check and the `trusty-mpm` bin running the daemon report the same id for one `cargo install`. The previous `mtime:size` fingerprint of each process's own executable differed between the two bins by construction, so the stale-daemon Warn never cleared on a fully current install (Refs #7873).
