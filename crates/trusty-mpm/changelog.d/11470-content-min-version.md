Fixed
- A content pin older than the release this `tm` needs is refused before any file is read, and the message names `tm content update` only when a qualifying release is published; otherwise it says none is. `tm fleet init` on a host pinned to content-v2.4.0 no longer fails with a missing `owner-plain.md` (#11470).
- `tm content update` no longer answers "already pinned and verified" for a pin below the minimum content release: it moves to the newest qualifying release, or says no release at or above the minimum is published. `--content-ref` below the minimum is refused (#11470).
