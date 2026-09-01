Documentation

- Fixed 13 broken intra-doc links in the `host_metrics` and `log_drain` module
  headers. Both headers are concatenated with the outer `///` block on their
  `pub mod` declaration in `lib.rs`, so rustdoc resolves them in the crate-root
  scope and bare item names never resolved. Link targets are now crate-qualified,
  matching the `sys_metrics` link that already worked. A broken link is baked
  into a docs.rs page permanently, so these had to land before publishing
  (#6549).
