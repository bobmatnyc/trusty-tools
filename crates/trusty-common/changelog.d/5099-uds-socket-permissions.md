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

Changed

- **BM25 and embedder sockets move into a per-uid directory**
  ([#5099](https://github.com/bobmatnyc/trusty-tools/issues/5099)).
  `bm25_client::socket_path_for_palace` and `UdsEmbedderClient::default_path`
  resolved to `$TMPDIR`, falling back to `/tmp` when `TMPDIR` was unset — mode
  `1777` and owned by root on a Linux host, so the socket was reachable by every
  local user and the directory could be neither narrowed nor trusted. Both now
  resolve under `uds::scratch_socket_dir()` — `<$TMPDIR or /tmp>/trusty-<uid>` —
  which the daemon holds at `0700`. **A running daemon must be restarted to be
  found at the new path**; the client and daemon resolvers changed together.
  The extra path segment costs 11 bytes of the kernel's `sun_path` budget (104
  on macOS, where `$TMPDIR` alone is ~50), narrowing the usable palace-name
  length from roughly 37 characters to roughly 26.
