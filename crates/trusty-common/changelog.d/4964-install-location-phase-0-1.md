Fixed

- Added `bin_resolve::canonical_bin_dir` (and its pure
  `canonical_bin_dir_from`) — one implementation of "where does `cargo install`
  put binaries", `$CARGO_HOME/bin` falling back to `~/.cargo/bin`. Five call
  sites across this crate and `trusty-installer` each restated the rule, and two
  restated it wrongly: one hardcoded `~/.cargo/bin` and never read `CARGO_HOME`,
  another treated `CARGO_HOME=""` as a real value and resolved the relative path
  `bin`. `update::candidate_bin_dirs` now derives its first entry from the
  shared helper rather than its own copy
  ([#4964](https://github.com/bobmatnyc/trusty-tools/issues/4964))
