Fixed
- `tm compress` unit tests that stub the `rtk` binary no longer fail intermittently: each stub gets a path unique per call, and a `/bin/sh` child writes it so no concurrent fork inherits a writable fd and hits `ETXTBSY` on exec. Test-only change (#10232).
