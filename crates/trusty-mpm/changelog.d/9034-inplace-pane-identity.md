Fixed
- Bare `tm` in a managed pane now relaunches the session in place when the daemon still reads it as active but its recorded tmux pane is this pane. An active session bound to a different pane still falls through to the guided picker.
