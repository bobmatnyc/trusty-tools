Added

- The `TM <ver>` text in `tm statusline` is now a clickable link to the
  trusty-console. Cmd-click (macOS) or Ctrl-click opens the dashboard on the
  port the console is actually selected for — read from its discovery file,
  falling back to the default `7788` when no console is configured. The link is
  an OSC 8 escape sequence, the form Claude Code's status line documents for
  clickable text, so the port lives in the escape and no rendered column gets
  wider or gains a port that #6304 removed. A terminal without hyperlink
  support shows the same `TM <ver> ●` text it showed before.
- `discovery::console_addr` and `discovery::console_base_url` are the single
  entry point for the console's selected address. The gateway resolver's
  inline discovery-file read moved into them, so the statusline and
  `resolve_daemon_url_via_gateway` now resolve the console the same way instead
  of keeping two copies of the fallback rule. Neither probes the network.
