Changed
- A fresh managed PM launch creates its tmux session on its workspace's private tmux server and stores that server's socket on the session record; the runtime launch, resume, task injection and reuse checks follow it there. `TM_WORKSPACE_TMUX=0` keeps the default-server launch for one release. Existing sessions and the Architect stay where they are (#10101).
- Resuming a session whose workspace tmux server crashed restarts that server and recreates the pane instead of refusing the resume (#10101).
- Stopping a session on a workspace tmux server reads its `claude` pid on that server, so the stop sends SIGTERM rather than only Ctrl-C (#10101).
- Pane capture, input-box reads and command sends by session name reach the one tmux server that holds the session, and refuse a name two servers hold; the pane-id backfill, the tmux environment heal and the reactivate foreground check use the record's server (#10101).
- The orphan GC sweeps every live workspace tmux server, and boot adoption adopts a session found on one with that server's socket (#10101).
- `tm` attach outside tmux attaches with `-S <socket>` to a session on a workspace server; inside tmux, a session on another server opens a new window in the caller's session that runs the attach, and a session on the same server is switched to as before (#10101).
