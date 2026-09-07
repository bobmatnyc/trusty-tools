Added

- The repo's issue lifecycle model now declares `status:merged -> status:coded`, so a merged issue whose live verification failed and which drew a follow-up fix PR is tracked as coded again instead of sitting at `status:merged`. Before this, `tm issue transition N status:coded` from `status:merged` was refused with exit 1 — the model offered only `status:tested` and `open`. The `tm-ticketing` skill states when to take the edge (owner ruling 2026-09-07).
