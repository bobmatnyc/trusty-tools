Fixed
- `tcode tui` refuses to attach to a daemon whose build differs from its own, or that reports no build, with a message naming both builds, the daemon's pid and the restart steps. The daemon's `tcode serve: starting` log entry now carries a `build` field (#8203).
