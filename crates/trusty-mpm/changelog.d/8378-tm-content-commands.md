Added
- `tm content install --from <bundle.tar.gz>` installs a content bundle
  offline, `tm content update [--content-ref content-vX.Y.Z]` fetches a
  release and its `.sha256` sidecar from GitHub, and `tm content status` shows
  the content source (`dev`, `bundle` or `none`), the pinned tag and sha256,
  and the binary version (ADR-0064, #8378). A bundle is pinned only after it
  matches its sidecar and passes the runtime resolver's own checks; it is
  stored in `~/.trusty-mpm/content` before `content-lock.toml` names it, and
  every write holds an exclusive lock on `.update.lock`. The sidecar is
  required by both verbs; it proves the transfer only, and trust is on first
  use (the pinned sha256 is what later reads check), as both verbs' `--help`
  states. `update` with no flag installs the newest published release —
  neither a draft nor a pre-release in GitHub's releases API, not a bare
  `content-v*` git tag — and re-pins to it; the pin changes only when a
  `tm content` command runs. A sha256 mismatch, a missing sidecar, a tag
  missing upstream, a re-fetch of the currently pinned tag that returns other
  bytes (only the current pin is checked), a newer or missing `schema_major`, a
  failed releases listing or an unreachable host fails without touching the
  previous pin; with nothing installed, the error names
  `tm content install --from <bundle.tar.gz>`. `tm content status` with
  nothing installed prints the doctor's `info:` line and exits 0 while `tm`
  still compiles its content in, and exits non-zero once ADR-0064 PHASE_1
  removes it.
- `tm doctor` gains a `content` row: OK for a dev checkout or a verified
  bundle; with nothing installed and no checkout serving, INFO (an `Ok` row
  whose message starts `info:`) while `tm` still compiles its content in, and
  WARN once ADR-0064 PHASE_1 removes it; and, when the lock or the bundle
  fails verification, FAIL if no checkout serves or WARN if a dev checkout
  does. The remedy it names fits the failure: a broken lock or a too-new
  schema names `--content-ref` (and, for the schema, upgrading `tm`), not a
  plain `tm content update`.
