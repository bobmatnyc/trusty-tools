Fixed

- `tm events` now shows the PR watch's `ci_transition` events. The daemon keeps the events it emits itself in a bounded ring served at `GET /events/daemon/poll` (`mpm.events.daemon_poll`), and `tm events` prints them after the hook feed. A watch emit the stopped bus buffer refuses is now an emit error, so the watch retries it instead of recording it delivered (Refs #12049).
