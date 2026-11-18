Changed
- A fresh trusty-mpm session now launches on the default tmux server. `TM_WORKSPACE_TMUX=1` opts in to the per-workspace `tm-ws-*` server.
- Resuming a stopped session whose workspace server holds no live pane recreates its pane on the default tmux server, keeping its project, account and Claude conversation. A live pane is never moved, and `TM_WORKSPACE_TMUX=1` keeps a session on its workspace server.
