Changed

- **The embedder UDS socket path moves into a per-uid directory**
  ([#5099](https://github.com/bobmatnyc/trusty-tools/issues/5099)).
  `embedder_supervisor::default_socket_path` resolved to `$TMPDIR`, falling back
  to `/tmp` on headless Linux — world-writable, and unable to be narrowed to
  `0700`. It now resolves under `trusty_common::uds::scratch_socket_dir()`
  (`<$TMPDIR or /tmp>/trusty-<uid>/`), keeping the PID suffix that separates
  concurrent daemons. Affects the `TRUSTY_EMBEDDER=unix:/path` transport only;
  the default auto-spawn path uses stdio and is unchanged.
