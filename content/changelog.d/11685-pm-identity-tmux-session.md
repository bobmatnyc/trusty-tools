Fixed
- The PM output styles take the session identity from the tmux session of `$TMUX_PANE`, never the folder name, and use it for the Intent Ledger `--session`, relay senders and claims; several PMs can share one folder (#11685).
