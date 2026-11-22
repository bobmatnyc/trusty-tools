Fixed
- The scheduler's default shadow argv is now `tm architect poll --dry-run --shadow`, so the shadow cycle runs the ledger shim like the live poller, and a shadow dry run passes its own `ARCHITECT_DRY_RUN_STATE` file to the shim as `--state` instead of the once-seeded copy (MB1595 fix round).
