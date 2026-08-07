Added

- **New `uds` module: the `0600` socket permission ADR-0031 and ADR-0032 both
  cite as an existing property** ([#5099](https://github.com/bobmatnyc/trusty-tools/issues/5099)).
  No production code in the workspace set permissions on any socket — every
  `set_permissions` / `PermissionsExt` hit was a test fixture — so sockets were
  created at the process umask (commonly `0755`). `uds::bind_hardened` is the
  single entry point every bind site now routes through: it creates the
  containing directory at `0700` via `DirBuilder::mode()` (passed to `mkdir(2)`,
  so the directory is never observable at a wider mode) and narrows the socket
  to `0600` before the caller's first `accept`. `uds::ensure_peer_is_self`
  refuses any connection whose uid is not this process's own, via `SO_PEERCRED`
  on Linux and `getpeereid` on macOS/BSD, which is what makes the permission
  bits an enforced boundary rather than a documented intention. Gated behind a
  new `uds` feature, implied by `bm25-client` and `embedder-client`; adds no new
  dependency (`libc` moves from a macOS-only to a `cfg(unix)` target dependency).
