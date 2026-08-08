Fixed

- **The daemon socket was world-readable and lived in a shared directory**
  ([#5099](https://github.com/bobmatnyc/trusty-tools/issues/5099)).
  `bind_listener` called `UnixListener::bind` bare, leaving the socket at the
  process umask, and `default_socket_path` resolved to `$TMPDIR` falling back to
  `/tmp` — world-writable on a Linux host with `TMPDIR` unset. The bind now goes
  through `trusty_common::uds::bind_hardened` (`0700` directory, `0600` socket),
  the path resolves under `<$TMPDIR or /tmp>/trusty-<uid>/`, and `run_accept_loop`
  drops any connection whose peer uid is not this process's own. **A running
  daemon must be restarted** to be found at the new path.
