Changed
- A daemon SIGTERM or restart no longer stops managed sessions: the graceful shutdown leaves every session, its tmux pane and its record as they are, and the restarted daemon re-adopts them at boot. A session still ends on `tm session stop`, `DELETE /sessions/{id}`, the orphan-GC, or a spawn rollback (#10597).
