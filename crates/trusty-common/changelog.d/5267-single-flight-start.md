Added
`mcp::ensure_daemon_up_single_flight` and `mcp::StartLock`: start a daemon at most once across concurrent processes, using an exclusive `flock(2)` that the kernel releases if the starter dies. Additive — `DaemonBridgeConfig` and `ensure_daemon_up` are unchanged.
