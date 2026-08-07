Fixed

- `tm ls` no longer staggers `NAME`/`TASK`/`CREATED` on rows whose state carries an annotation
  - the `STATE` column was a hardcoded 14 characters wide, but its rendered value is the state plus any annotation — `attached [stale-assets]` is 23 — so every annotated row pushed the three columns after it nine places right
  - the width is now measured across the rows actually being printed, floored at the historical 14 so an unannotated listing keeps its shape
