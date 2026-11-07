Changed
- A fresh managed PM launch creates its tmux session on its workspace's private tmux server and stores that server's socket on the session record; the runtime launch, resume, task injection and reuse checks follow it there. `TM_WORKSPACE_TMUX=0` keeps the default-server launch for one release. Existing sessions and the Architect stay where they are (#10101).
- Resuming a session whose workspace tmux server crashed restarts that server and recreates the pane instead of refusing the resume (#10101).
