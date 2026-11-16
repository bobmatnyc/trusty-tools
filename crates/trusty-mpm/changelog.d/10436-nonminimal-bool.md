Fixed
- `cargo clippy -p trusty-mpm --all-targets -- -D warnings` passes again on the MSRV 1.94 toolchain: three `nonminimal_bool` conditions are rewritten with identical behavior (#10436).
