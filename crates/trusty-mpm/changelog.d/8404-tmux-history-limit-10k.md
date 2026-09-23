Changed

- A managed tmux session with no `tmux.history_limit` in `~/.trusty-tools/trusty-mpm/config.yaml` now gets 10,000 lines of scrollback, down from 100,000, which lagged every tmux pane on the host. Set `history_limit` to keep a larger value; values below 1,000 still clamp to 1,000 (Refs [#8404](https://github.com/bobmatnyc/trusty-tools/issues/8404)).
