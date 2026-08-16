Fixed
- `tctl install` no longer reports success when an all-OPTIONAL selection
  genuinely failed. The `all_ok` filter kept only REQUIRED members, and
  `.all()` over an empty iterator is vacuously true, so `tctl install tga` (or
  `trusty-analyze`, or `trusty-console`) printed `all_ok: true` and exited 0
  with nothing installed. A selection containing no REQUIRED member now gates
  on every member it selected (#5806).
- The post-install verify tail had the same vacuous truth one file over:
  `verified` collapsed to the `ensure` result for any selection whose daemons
  are all OPTIONAL, so `tctl install trusty-analyze trusty-console` printed
  VERIFIED with both daemons down. Both reports now read one shared gating rule
  (#5806).
- The human install summary contradicted the exit code. An all-optional
  selection that failed printed `installed 0/0 required component(s)`, an
  info-level "skipped", and a green VERIFIED while the process exited 2. The
  footer now counts the same gating set the exit code uses and prints those
  failures as errors (#5806).
- `InstallReport::build` over an empty member list reported `all_ok: true`.
  Nothing reaches it today — `run` returns early on an empty selection — but
  installing nothing is not evidence of a successful install (#5806).
