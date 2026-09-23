Fixed
- `tm sessions resume X` with `X` gone no longer reaches a live session whose name starts with `X`. The `has-session` probe, the resume runtime probe (`list-panes -s`), the daemon's `kill-session` and `display-message` pane lookups, `attach-session`, `switch-client` and `list-clients` now all use exact tmux targets. Before, `tm sessions resume tm-cto` killed `tm-cto-reports` (#8443).
- The resume runtime probe now reads a `can't find session` or `no server running` reply as "no runtime is live" instead of failing open, because an exact target makes that reply certain (#8443).
- The attach command the daemon returns (`attach_cmd`) and the hints `tm` prints are now `tmux attach-session -t '=<name>'`, quoted so zsh does not read the leading `=` as a command-path expansion (#8443).
