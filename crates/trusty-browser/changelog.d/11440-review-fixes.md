Fixed
- A persistent accept error (EMFILE, ENFILE, ENOBUFS, ENOMEM) makes the broker wait 1 s before it accepts again, as Python asyncio does, instead of spinning
- `install-service` over a loaded Python broker job boots it out and bootstraps the new plist, so the Python broker no longer keeps running
- A `test-seams` build (any `cargo test` or `--all-targets` binary) refuses `install-host` and `install-service` with "test build; refusing to register", and its `serve` prints a stderr banner when the seams are active
