Fixed
- `tm fleet relaunch` restarts a missing `tm-architect-poll` poller once the new `claude` is bound, so the daemon registration that follows records the `[architect-helper]` row again after a reboot. A poller already running is left alone; one running in another directory is a FAILED step (#10357).
- `tm fleet init` no longer starts the poller beside a running Architect that is not bound; the step is skipped and names `tm fleet relaunch` (#10357).
