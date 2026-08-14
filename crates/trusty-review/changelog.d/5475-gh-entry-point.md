Changed

- `SystemGhResolver` resolves `gh auth token` through
  `trusty_common::gh::GhCommand`
  ([#5475](https://github.com/bobmatnyc/trusty-tools/issues/5475)) instead of
  its own `Command::new("gh")`. The resolution order (`GITHUB_TOKEN` →
  `GH_TOKEN` → `gh auth token`) and the debug-log-and-return-`None` degrade are
  unchanged; the log line now carries the entry point's classified reason.
