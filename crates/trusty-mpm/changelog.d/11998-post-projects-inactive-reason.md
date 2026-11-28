Added
- `POST /projects` and `mpm.projects.register` on a decommissioned or archived project still leave it inactive, and the `200` response now carries `inactive_reason`, which says to start a session to reactivate it. The key is absent for an active project (#11998).
- `tm project init` on a decommissioned or archived project now prints a `note:` line with the daemon's reason and the way to reactivate the project: start a session (#11998).
