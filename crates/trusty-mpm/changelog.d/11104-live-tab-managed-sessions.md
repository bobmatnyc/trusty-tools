Fixed
- The Live tab of `tm tui` now lists managed sessions. It read only `mpm.sessions.list`, the hook registry, which holds no session the session manager spawned; it now also reads `mpm.managed.list` and adds each managed session the registry lacks, with its agents and step. A failed managed read fails the Live fetch with `mpm.managed.list: <error>` rather than drawing an empty list (#11104).
