Changed

- The four remaining read-only tmux probes (`tmux_attach::current_tmux_session_name`,
  `statusline::branch::tmux_session_name`, `guided_inplace::read_tmux_env_managed_session_id`,
  `core::process::tmux_pane_pid`) now route through `core::tmux`'s resolved
  binary + TCC-disclaimed spawn primitive instead of a bare, unresolved
  `Command::new("tmux")`, via two new `core::tmux` helpers
  (`display_message_argv`/`show_environment_argv` +
  `run_tmux_argv_with_bin`/`run_tmux_argv`) added alongside the existing typed
  `TmuxCommand` path. Targeting semantics are unchanged at every site.
