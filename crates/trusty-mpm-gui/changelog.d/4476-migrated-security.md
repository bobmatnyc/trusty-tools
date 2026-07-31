Security

- Set an explicit Content-Security-Policy (`default-src 'self'; connect-src 'self' ipc: http://ipc.localhost http://127.0.0.1:7880; style-src 'self' 'unsafe-inline'`) in `tauri.conf.json`, replacing `"csp": null` (architecture-review tranche 0). Added a hand-authored `capabilities/default.json` (`core:default` only, scoped to the `main` window) so the app's ACL is explicit rather than implicit.
