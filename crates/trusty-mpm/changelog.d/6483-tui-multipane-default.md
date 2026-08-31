Changed

- `tm tui` and `tm attach <target>` now open the multipane project control
  dashboard (projects / sessions / activity / action bar) instead of the
  single-pane coordinator chat. `tm attach` selects the resolved session's
  owning project and row on the first frame.
  - The coordinator chat stays reachable with the new `--single-pane` flag on
    both commands, and `tm sessions tui` is unchanged.
