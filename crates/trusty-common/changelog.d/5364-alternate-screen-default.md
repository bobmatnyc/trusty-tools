Changed

- `tmux::DEFAULT_TMUX_ALTERNATE_SCREEN` flipped from `true` to `false`
  ([#5364](https://github.com/bobmatnyc/trusty-tools/issues/5364)). Both
  consumers — trusty-mpm's config resolution and trusty-agents' two
  session-creation call sites — inherit it, which is what keeps them from
  fighting over the shared tmux server's global `alternate-screen` option.
  Managed sessions now get working scrollback by default; the cost is that
  `vim`, `less`, `htop` and `man` no longer restore the screen they covered.
  Callers passing an explicit value are unaffected.
