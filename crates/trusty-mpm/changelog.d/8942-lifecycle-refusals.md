Changed

- The daemon never stops, decommissions, prunes, resumes, deletes, idle-stops,
  nudges or injects a task into a live Architect session (kind `supervisor`
  or `supervisor_aux`). A stop answers "exit claude in its pane"; a resume
  points to `tm fleet init`. Daemon shutdown leaves those sessions running,
  and when the Architect's pane is gone its record is marked stopped without
  any teardown (#8942).
- Boot reconcile adopts a live pane that an Architect launch sidecar names as
  the Architect's record, under a stable id, never as an ordinary adopted
  session. When the sidecars cannot be read, no pane is adopted that pass.
- When the kill-by-name floor refuses a kill (including when it cannot read
  the sidecars or the store), `stop` and `decommission` now abort with the
  refusal and leave the workspace and the record untouched, instead of
  marking the record stopped or decommissioned while claude still runs.
- `POST /api/v1/sessions/managed/{id}/runtime-stop` answers a refused stop
  with 409 and the reason, instead of 404.
