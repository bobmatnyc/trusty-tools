Fixed
- The daemon `tm daemon start` and the guided-launch autostart spawn now starts in its own session, so Ctrl-C to `tm`'s foreground group or a group kill aimed at `tm` no longer kills it (#8783).
