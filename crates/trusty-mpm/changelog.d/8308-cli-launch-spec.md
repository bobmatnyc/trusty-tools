Fixed

- `tm launch`, `tm connect`, the in-place `tm session start`, the TUI's
  `DaemonClient` launch and connect, and the Architect launch in `tm fleet`
  now start `claude` from a mode-0600 launch spec file, as the daemon's
  managed launch has since #8233. The pane types only
  `'<tm>' internal-spawn-disclaimed --launch-spec '<file>'`, so the typed line
  stays short (it reached 1229 bytes with an OAuth token and a supervisor's
  divert settings) and `CLAUDE_CODE_OAUTH_TOKEN` no longer appears in the
  pane's scrollback.
