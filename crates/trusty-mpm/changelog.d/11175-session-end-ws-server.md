Fixed
- A `SessionEnd` hook no longer marks a session on a `tm-ws-*` workspace tmux server Stopped while its pane still runs the runtime, so `tm projects decommission` stops that session and its pane (#11175).
