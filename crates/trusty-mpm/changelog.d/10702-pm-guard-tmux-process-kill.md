Fixed
- `tm hook --pm-guard` refuses a kill of the tmux server by process (`pkill tmux`, `killall tmux`, `kill $(tmux display -p '#{pid}')`, `pgrep tmux | xargs kill`); a scratch `-L`/`-S` server and unrelated kills still run.
- The same guard reads the kill behind a wrapper (`timeout 5`, `sudo -u root`, `env -u X`), a compound head (`(`, `{`, `do`, `then`), a nested `$(...)`, backtick, `eval` or `find -exec`, and a `ps ... | xargs kill` pid list; a scratch server excuses only the operand that names it.
