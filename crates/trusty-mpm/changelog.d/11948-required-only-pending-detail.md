Fixed
- `tm wait --for check --required-only` now names only the required contexts that keep it pending, and marks one that has not registered yet, e.g. `1 of 8 required unsettled: CI gate (not registered)`. It used to list every required context, which read as "all pending" while `gh pr checks --required` showed most passed (#11948).
