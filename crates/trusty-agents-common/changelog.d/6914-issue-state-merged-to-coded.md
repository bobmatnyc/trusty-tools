Added

- The ticketing agent takes `tm issue transition N status:coded` when live verification of a merged fix fails and a follow-up fix PR opens. The lifecycle model gained the `status:merged -> status:coded` edge that move needs; before it, the agent had no state to move the issue to and left it at `status:merged` (owner ruling 2026-09-07).
