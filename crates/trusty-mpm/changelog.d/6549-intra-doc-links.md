Documentation

- Fixed 6 broken intra-doc links in the daemon `log_drain` module header. The
  header is concatenated with the outer `///` block on `pub mod log_drain;` in
  `daemon/mod.rs`, so rustdoc resolves it in the crate-root scope and bare item
  names never resolved; `log_drain_loop`, `drain_once`, `LogDrainStatus`,
  `DrainOutcome` and `DrainOutcome::Failed` are now crate-qualified.
  `orphan_gc_loop` is private, so it reads as plain backticks rather than a link
  (#6549).
