Fixed
- The guided-fallback tests no longer leave `tm-<uuid>` tmux sessions on the operator's live tmux server. Every test that drives the daemon-unreachable fallback now launches its session on a private `tmux -L` server that the test kills when it ends, through a new task-scoped `core::tmux::with_tmux_binary` override (#6542).
