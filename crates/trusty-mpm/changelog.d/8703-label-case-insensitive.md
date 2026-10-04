Fixed

- `tm issue transition`, `current`, `repair` and the epic tracker resolve an issue's state from a label that differs from the model's `label.name` only in case, as GitHub treats label names (#8703). `tm issue seed-labels` counts such a label as present instead of trying to create it again.
