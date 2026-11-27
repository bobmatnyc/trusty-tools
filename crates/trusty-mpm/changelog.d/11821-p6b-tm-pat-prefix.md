Fixed
- `tm pat <alias>` runs `tm path` again. Adding `tm paths` had made `pat` a prefix of both commands, so it no longer resolved to `path`; no `pa`/`pat` prefix reaches the owner-only `tm paths set` (#11821).
