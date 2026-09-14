Documentation

- `tm-delegation-patterns` PM Re-Engagement now states that a handed-over
  branch may not be green — the agent's own gate can pass while CI fails —
  and that the PM re-engagement brief runs the full gate first (#7919).
- `tm-delegation-patterns` Worktree Isolation now notes that concurrent
  `cargo` invocations in one checkout contend on the `target/` build lock
  with no signal to either agent, and that `isolation: "worktree"` avoids it
  (#7895).
- `test_support.rs` and `test-ladder-baseline.md` document that a
  `--path`-included test module filters by its compiled module path
  (`tmux_session`), not its source basename (`test_tmux_session`), with the
  working `cargo test` invocation (#7866).
