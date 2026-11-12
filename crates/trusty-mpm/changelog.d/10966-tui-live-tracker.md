Added
- `tm tui`: a Live tab in the Architect view, opened with `v` from the home screen, lists every session top to bottom with its running agents and, for the selected session, its current step (`-` for an unmanaged session). It reads `mpm.sessions.list`, `mpm.delegation.list` and `mpm.managed.activity` over the daemon socket only, and is a list with no chart.
- `tm tui`: the Live tab refreshes on its own schedule and never waits on the dashboard fetch; a failed tracker read replaces the earlier rows with the error.
