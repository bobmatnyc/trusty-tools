Fixed

- `tm hook`'s `PreToolUse` Bash rewrite no longer appends
  `| tm compress --tool "<name>"` to commands no compression filter covers.
  `git status`, `sed`, `gh pr`, `wc`, `ssh` and the like each paid a process
  spawn to get their own bytes back: 3,415 of 3,643 wrapped invocations
  (93.7%) over a measured 48-hour window reduced nothing. The rewrite now asks
  `trusty_agents_common::compress::has_filter_for` before wrapping. Commands
  the dispatch does cover — `cargo test`, `cargo check`/`clippy`, `git diff`,
  `git log`, `grep`/`rg`/`find`, `ls`, file reads — are wrapped exactly as
  before.
