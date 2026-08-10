Changed

- `tmux.alternate_screen` now defaults to `false`, so a tm-managed session has
  working scrollback out of the box ([#5364](https://github.com/bobmatnyc/trusty-tools/issues/5364)).
  [#5151](https://github.com/bobmatnyc/trusty-tools/issues/5151) added the knob
  to fix missing scrollback but shipped it defaulting to `true` — tmux's factory
  value and the pre-fix behaviour — so the original bug persisted for every
  operator who never found the knob. A full-screen TUI draws into tmux's
  alternate screen, and tmux does not append that output to the pane's
  scrollback at all; `history-limit` is irrelevant in that state.
- **Tradeoff, accepted deliberately:** `alternate-screen` is server-global and
  every trusty-* managed session shares one tmux server, so `vim`, `less`,
  `htop` and `man` no longer restore the terminal's prior screen on exit
  server-wide — each leaves its final frame in the scrollback. Set
  `tmux.alternate_screen: true` in `~/.trusty-tools/trusty-mpm/config.yaml` to
  restore the old behaviour.
