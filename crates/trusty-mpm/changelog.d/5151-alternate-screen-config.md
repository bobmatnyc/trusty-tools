Added

- `tmux.alternate_screen` in `~/.trusty-tools/trusty-mpm/config.yaml`, alongside `history_limit` and `mouse` ([#5151](https://github.com/bobmatnyc/trusty-tools/issues/5151)). It defaults to `true`, tmux's factory value and today's behaviour; setting it to `false` stops panes using the terminal's alternate screen buffer, which is what makes a full-screen TUI's output land in scrollback instead of being discarded. This is why a `tm` session can verify a 100,000-line `history-limit` and still have nothing to scroll back through — the history was never written to the pane.

  Turning it off is server-wide in effect: every trusty-* managed session shares one tmux server, so `vim`, `less`, `htop` and `man` also stop restoring the screen they covered, each leaving its final frame smeared into the scrollback on exit. It is not retroactive either — anything already written while the alternate screen was up stays unrecoverable.

  The option rides the existing verified apply path (`start-server` → `set-option` → `show-options` readback, retried, before `new-session`), and its readback queries the same `-wg` scope the set writes, so a wrong-scope set cannot verify green.
