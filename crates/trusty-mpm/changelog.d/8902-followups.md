Fixed

- `tm hook --pm-guard`'s Architect pane floor judges a nested `tmux` with no
  `-L`/`-S` in keys that `send-keys` types on another server against that
  server and the default one, because the pane's process may lack `TMUX`. An
  omitted target on a server other than the one the keys go to is a pane tmux
  picks, and it denies where the Architect is (#8902).
- The floor counts zsh's `${TMUX::=…}`, a quote-split name
  (`export TM''UX=…`), and an assignment through a name it cannot read
  (`${(P)n::=…}`, `${!n:=…}`, `export "$n=…"`, `unset "$n"`,
  `printf -v "$n"`, `eval "$x"`) as a `TMUX` change, so the tmux command
  denies while an Architect is live (#8902).
