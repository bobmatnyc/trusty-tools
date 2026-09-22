Changed

- `DEFAULT_TMUX_HISTORY_LIMIT` is now 10,000 lines, down from 100,000. The larger value, with a long pane history, gave every tmux pane on the host seconds of lag per keystroke. `tmux.history_limit` in `~/.trusty-tools/trusty-mpm/config.yaml` still overrides it, with the 1,000-line floor unchanged (Refs [#8404](https://github.com/bobmatnyc/trusty-tools/issues/8404)).
