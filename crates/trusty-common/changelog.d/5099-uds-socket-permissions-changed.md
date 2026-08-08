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
