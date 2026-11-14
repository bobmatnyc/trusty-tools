Fixed
- `tm`'s test suite: the session-resume tests now resolve the tmux holder under a socket directory of their own, so a concurrent test's probe of the default server no longer makes the dead-runtime resume test fail under a single-process `cargo test`. Non-test builds still use tmux's socket directory unchanged (Refs #11223).
