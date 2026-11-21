Fixed
- `tm hook --pm-guard` refuses a kill of the tmux server by process (`pkill tmux`, `killall tmux`, `kill $(tmux display -p '#{pid}')`, `pgrep tmux | xargs kill`); a scratch `-L`/`-S` server and unrelated kills still run.
- The same guard reads the kill behind a wrapper (`timeout 5`, `sudo -u root`, `env -u X`), a compound head (`(`, `{`, `do`, `then`), a nested `$(...)`, backtick, `eval` or `find -exec`, and a `ps ... | xargs kill` pid list; a scratch server excuses only the operand that names it.
- The kill reader matches `pkill`/`killall`/`kill` only in command position, so `grep kill tmux.log` runs, and it judges each `;`, `&&`, `||` or newline segment alone.
- The guard also splits on a single `|` and a bare `&`, reads `if`/`while`/`until` heads, and refuses a kill behind an unlisted wrapper or leading redirect (`watch pkill tmux`, `>/dev/null pkill tmux`) unless the segment opens with a data program such as `grep`.
