Fixed
- A bare `tm` in a supervisor session's pane no longer relaunches `claude` in place, which left the Architect with no launch record and its anchor writes denied. It exits 1 and points at `tm fleet relaunch` (#9092).
