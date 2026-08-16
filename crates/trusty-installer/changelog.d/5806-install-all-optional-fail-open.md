Fixed
- `tctl install` no longer reports success when an all-OPTIONAL selection
  genuinely failed. The `all_ok` filter kept only REQUIRED members, and
  `.all()` over an empty iterator is vacuously true, so `tctl install tga` (or
  `trusty-analyze`, or `trusty-console`) printed `all_ok: true` and exited 0
  with nothing installed. A selection containing no REQUIRED member now gates
  on every member it selected (#5806).
