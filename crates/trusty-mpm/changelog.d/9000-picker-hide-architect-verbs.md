Fixed
- The `tm ls` session TUI no longer offers open (resume) or delete on the Architect's rows and its helpers' rows. Enter and `d` on such a row send no request and show the daemon's own refusal; the footer and the `?` help drop both verbs and say why. Ordinary rows, and an Architect record already decommissioned or deleted, keep every verb (#9000).
