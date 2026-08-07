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
