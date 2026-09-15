Fixed
- Launch no longer probes trusty-memory twice: the deployment gate's auto-repair reuses the reachability the same launch already resolved, so a slow or wedged daemon costs one probe timeout instead of two (#7763).
- `session_context_catchup` reports `resolved_snapshot_superseded` with the commits that landed since the snapshot's recorded commit, so a resuming PM does not re-plan work that already merged (#7501).
