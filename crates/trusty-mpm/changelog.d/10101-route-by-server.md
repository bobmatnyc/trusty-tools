Changed
- Session pane reads, sends, teardowns, renames and resumes reach the tmux server a session record names (`tmux_socket`), and refuse when that server cannot be reached instead of using the default server (#10101).
- Session liveness is keyed by tmux server and session name: a same-named session on another server no longer keeps a record live, and an unreachable server makes liveness unknown (#10101).
- `tm doctor`'s `tmux_options` row reports each live workspace tmux server beside the default one; the tmux snapshot refuses a name that two servers hold (#10101).
- The Architect fleet poller never takes a workspace server's same-named session for the Architect's pane (#10101).
