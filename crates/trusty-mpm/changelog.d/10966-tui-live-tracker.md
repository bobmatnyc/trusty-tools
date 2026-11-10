Added
- `tm tui`: a Live tab in the Architect view, opened with `v` from the home screen, lists every session top to bottom with its running agents and, for the selected session, its current step (`-` for an unmanaged session). It reads `mpm.sessions.list`, `mpm.delegation.list` and `mpm.managed.activity` over the daemon socket only, and is a list with no chart.
