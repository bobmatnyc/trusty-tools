Fixed

- Running `cargo test -p trusty-mpm` no longer leaves a live `tm-<uuid>` tmux
  session behind per run. The two guided-fallback tests drive
  `fallback_protected`, which launches a real session in the worktree it
  provisions; nothing killed it, so 456 accumulated on one machine over five
  days and exhausted its pseudo-terminal pool. The test-support fixture now
  claims such a session by its pane's working directory and kills it on drop,
  panicking assertions included (#6542, refs #6523).
</content>
</invoke>
