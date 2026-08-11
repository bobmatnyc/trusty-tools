Changed

- `tagent`-created tmux sessions now set `alternate-screen off`, inheriting the
  `DEFAULT_TMUX_ALTERNATE_SCREEN` flip in
  [#5364](https://github.com/bobmatnyc/trusty-tools/issues/5364). Both
  session-creation call sites (`tmux::orchestrator`, `debugger::tmux`) pass the
  shared constant through unchanged, so they stay consistent with trusty-mpm on
  the tmux server both drive. Full-screen programs no longer restore the
  terminal's prior screen on exit; their output lands in scrollback instead.
