Changed
- Every `tm` command, hook, the MCP stdio bridge (`tm serve --stdio`), the TUIs, the Telegram and Slack bots, `tm sessctl` and `TmConnector` reach the daemon over its unix socket only. No client resolves `http://127.0.0.1:7880`, the lock file's address or the console gateway, and none falls back to TCP: an absent, stale or unreachable socket is an error naming the socket path (#6288).
- `--url` and `TRUSTY_MPM_URL` no longer select a daemon. A loopback URL is ignored with one stderr notice; a URL naming another host is refused before the command runs. `tm hook` prints the refusal and runs with no daemon, so a hook never fails the turn (#6288).
- `tm sessctl connect` reads the session's events from the daemon's `mpm.control.connect` frame stream. The socket form observes only and takes no write-lock (#6288).
- The daemon serves the egress guard's verdict (`mpm.channels.egress_verdict`) and a managed session's file changes (`mpm.managed.files`) on its socket, with the same answers as their HTTP routes (#6288).
- `tm hook` no longer retries a `SessionStart` over HTTP when the socket is unreachable; the delivery counts as unconfirmed (#6288).
