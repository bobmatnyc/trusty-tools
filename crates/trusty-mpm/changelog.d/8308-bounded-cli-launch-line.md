Fixed

- `tm launch`, `tm connect`, the in-place `tm session start`, the TUI's
  `DaemonClient` launch and connect, and the Architect launch in `tm fleet` now
  type their launch line through the pane-command length guard
  (`MAX_PANE_COMMAND_BYTES`, 960 bytes). A line over the limit is refused with
  an error that names its size, instead of being cut off by the tty with no
  error. `tm launch`/`tm connect` ask you to report a failed start with
  `tm doctor` output.
