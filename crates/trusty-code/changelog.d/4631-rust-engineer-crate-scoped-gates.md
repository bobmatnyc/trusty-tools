Changed

- Embedded `rust-engineer` agent asset re-synced with its trusty-mpm source,
  which now scopes the Quality Bar to the crate under change
  (`cargo test -p <crate>`) and states the "scope is for speed, never for hiding
  a failure" rule. Byte-parity with `crates/trusty-mpm/src/assets/agents/` is
  preserved.
