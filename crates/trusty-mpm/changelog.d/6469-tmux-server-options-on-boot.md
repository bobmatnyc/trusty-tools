Fixed

- **A restored tmux server gets tm's scrollback settings back.** A tmux server
  restart with tmux-continuum restore recreates every `tm-*` session through
  tmux-resurrect's own bare `new-session`, which never passes through tm's
  session-creation choke point — so the server carried none of the
  `history-limit`, `mouse` or `alternate-screen` options tm specifies, and
  restored panes sat on tmux's factory 2000-line scrollback. The daemon now
  re-asserts those server globals when it reconciles sessions at boot, and a new
  `tmux_options` doctor check reports drift between the live server and tm's
  spec — warning per option, and reporting UNKNOWN rather than OK when no option
  could be read. A green check means NEW panes will be correct: `history-limit`
  is captured when a pane is created and cannot be grown in place, so an
  affected session still has to be restarted
  ([#6469](https://github.com/bobmatnyc/trusty-tools/issues/6469))
