Added
- `tm content install --from <bundle.tar.gz>` installs a content bundle
  offline, `tm content update [--content-ref content-vX.Y.Z]` fetches a
  release and its `.sha256` sidecar from GitHub, and `tm content status` shows
  the content source (`dev`, `bundle` or `none`), the pinned tag and sha256,
  and the binary version (ADR-0064, #8378). A bundle is pinned only after it
  matches its sidecar and passes the runtime resolver's own checks; it is
  stored in `~/.trusty-mpm/content` before `content-lock.toml` names it, and
  every write holds an exclusive lock on `.update.lock`. `install` needs the
  `<bundle>.sha256` sidecar beside the bundle. `update` with no flag installs
  the latest release when nothing is installed, and otherwise keeps the
  recorded pin, reporting a newer release without applying it. A sha256
  mismatch, a tag missing upstream, a republished tag or an unreachable host
  fails without touching the previous pin; with nothing installed, the error
  names `tm content install --from <bundle.tar.gz>`.
- `tm doctor` gains a `content` row: OK for a dev checkout or a verified
  bundle, WARN when nothing is installed and no checkout serves, and, when
  the lock or the bundle fails verification, FAIL if no checkout serves or
  WARN if a dev checkout does. The remedy it names fits the failure: a
  broken lock or a too-new schema names `--content-ref` (and, for the
  schema, upgrading `tm`), not a plain `tm content update`.
