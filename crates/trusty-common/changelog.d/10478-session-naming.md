Fixed
- `build_session_name` strips a leaf's own leading `tm-` and trailing `-NN` before adding the prefix, so a hint of `tm-mpm-bugs-01` gives `tm-mpm-bugs`, not `tm-tm-mpm-bugs-01-01`. `leaf_slug_from_hint` applies the same strip, so the picker preview matches the daemon (#10478).
- The first session for a leaf is bare `tm-<leaf>`. A `-02`, `-03`, … counter is added only when a taken name already holds `tm-<leaf>` or `tm-<leaf>-NN`; an existing `-01` counts as holding the base. Existing sessions keep their names (#10478).
