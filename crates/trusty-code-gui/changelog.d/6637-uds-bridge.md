Changed

- The desktop shell now serves the webview's HTTP itself, over a loopback bridge that dials the `tcode` daemon's Unix socket, instead of pointing the webview at the daemon's TCP listener.
- The bridge binds an ephemeral port and mints its credential per launch, handing both to the webview over Tauri IPC — so `TRUSTY_CODE_URL`, the hardcoded `127.0.0.1:7882` default, and the reliance on the daemon's `0600` token file are all gone.
