Fixed
- `tm hook --pm-guard` refuses a kill of the tmux server by process (`pkill tmux`, `killall tmux`, `kill $(tmux display -p '#{pid}')`, `pgrep tmux | xargs kill`); a scratch `-L`/`-S` server and unrelated kills still run.
