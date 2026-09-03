Fixed
- Test-only: `TRUSTY_DATA_DIR_OVERRIDE` is now guarded by one crate-wide lock instead of two, so a `remove_var` in the `lib.rs` port tests can no longer land inside the connector tests' critical section and send `detect()` at the live daemon on 7880 (#6661).
- Test-only: the closed-port probe assertion targets a privileged, non-ephemeral loopback port rather than a just-freed ephemeral one a parallel test can be handed (#6661).
