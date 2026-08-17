Added

- **The launch path warns when a checkout carries another harness's (claude-mpm) hook wiring**, reusing `tm doctor`'s own `hooks_foreign_conflict` detector so the finding lands when the session starts rather than only when someone runs the diagnostic. Report-only — tm never removes another harness's hooks
