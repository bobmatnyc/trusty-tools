Fixed

- `report` reads `TRUSTY_AUDIT_INVESTIGATE_MAX_FILES` and
  `TRUSTY_AUDIT_INVESTIGATE_MAX_BYTES` as the tier below the manifest's
  `[report].investigate_max_*` keys. `trusty-audit` records the budget it wants in
  the manifest, but `tga audit` has already run this binary against that manifest
  by the time the key is written — so an audit declaring 240 files rendered its
  report under this crate's 40-file default and said nothing. An explicit
  `--investigate-max-files` and a declared manifest key both still win; a
  non-numeric, zero or negative variable reads as absent.
