Changed

- **The webview bridge no longer opens a TCP port (#10166).** REST calls reach
  the in-process router over the `tcode-bridge://localhost` custom URI scheme,
  and the two event streams over a Tauri IPC channel (`open_daemon_stream` /
  `close_daemon_stream`), so the app binds no loopback listener. The per-launch
  credential and the single-use stream ticket are unchanged.
- The plain-browser mode is gone: the `trusty-code.daemonUrl` and
  `trusty-code.daemonToken` `localStorage` overrides are no longer read, and the
  `bridge_serve` example is removed. The app supports macOS only.
- The credential-attaching `fetch` compares protocol and host, because a
  custom scheme's URL origin is the opaque `"null"` and would otherwise match
  any other custom-scheme URL.
- A page reload closes every open event stream, so the old page's daemon
  connections no longer outlive it.
