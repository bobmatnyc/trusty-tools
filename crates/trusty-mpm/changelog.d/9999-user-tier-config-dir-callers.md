Fixed
- The daemon's deployment repair, its resume statusLine write, `tm session start` and the connect client's session prep now write the user-tier `settings.json` and `.claude.json` under `CLAUDE_CONFIG_DIR` when it is set, through the same resolver `tm validate --repair` uses, instead of under `$HOME/.claude` (#9999).
