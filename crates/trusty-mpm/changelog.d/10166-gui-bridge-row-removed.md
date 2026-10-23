Changed

- `tm doctor`'s `tcp_listeners` allowlist no longer excuses
  `crates/trusty-code-gui/src/bridge/serve.rs`: the GUI's webview bridge binds
  no TCP port since #10166.
