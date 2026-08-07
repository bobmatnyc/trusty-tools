Fixed

- **`prepare_socket_dir` followed symlinks, which defeated the whole scheme**
  ([#5099](https://github.com/bobmatnyc/trusty-tools/issues/5099) review finding
  1). `std::fs::metadata` and `set_permissions` both resolve links, so when the
  socket directory already existed as a symlink the owner and mode checks read
  the *target's* inode and the chmod retargeted it. An attacker who pre-created
  `trusty-<uid>` as a link to any directory the running user owns passed the
  ownership check outright — verified on macOS: `mkdir` returned `EEXIST`,
  `metadata()` reported uid 502 / mode 0777 from the target, and
  `set_permissions` chmod'd the target to 0700. Directory verification now uses
  `symlink_metadata` and refuses a symlink before considering owner or mode. The
  residual post-check swap race is not closed — it needs `openat`/`fchmod` on a
  directory fd, and `/tmp`'s sticky bit makes it impractical — and the module
  documentation now states that boundary instead of claiming more than it holds.
- **A non-directory at the socket-directory path got chmod'd to `0700`.**
  `classify_existing_dir` asserted symlink and ownership but never the file
  type, so a regular file owned by this uid was classified `Narrow`, had its
  mode rewritten, and only then failed `ENOTDIR` at `bind` — mangling a file
  this process did not create and naming neither the cause nor what was
  actually there. It now refuses with `NotADirectory`, which reports the type
  `lstat` found, before anything can chmod it. Both `prepare_socket_dir` and
  `connect_hardened` apply the check.
- **A dialer reported "create socket directory …" while creating nothing.** A
  failed `symlink_metadata` on the connect path mapped to `CreateDir`; it now
  uses a dedicated `StatForConnect` variant.
