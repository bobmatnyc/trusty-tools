Fixed

- **`tm doctor` no longer reports a trusty-memory daemon healthy when nothing is watching its palace locks.** A daemon whose palace-lock stall detector stopped reports `status: degraded`, which read as a warning and left the run green on the detector's own failure; a daemon predating the detector omits `worker.stall_tracking_ok` entirely while still reporting `worker.wedged`, so the 2026-09-13 incident build read HEALTHY. Both are now UNKNOWN, matching `trusty-memory doctor` ([#4001](https://github.com/bobmatnyc/trusty-tools/issues/4001))
