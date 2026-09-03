Fixed
- `spawn_claude_without_tmux_is_unprocessable_when_tmux_missing` pins `$HOME` at
  a tempdir and runs `#[serial]`, so it no longer depends on when a sibling test
  moves `$HOME` process-wide. `spawn_claude` reads the #5784 host-state gate
  twice and the test's own tmux probe read it a third time; a sibling landing
  between those reads made the call refuse against a scratch home while the
  probe answered against the real one, and the test saw `Internal` where it
  expected `Unprocessable`
  ([#6736](https://github.com/bobmatnyc/trusty-tools/issues/6736)). Test-only —
  no behavior changed.
