Added
- `tm fleet relaunch` restarts the Architect's `claude` in its running tmux session with the flags `tm fleet init` uses, records the new process as the Architect, removes the replaced launch record and re-registers with the daemon, so the Architect's anchor writes are allowed again. With no session running it exits 1 and names `tm fleet init` (#9092).
