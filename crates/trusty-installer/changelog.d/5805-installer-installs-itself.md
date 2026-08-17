Added
- `trusty-installer` is a member of its own stable set, so
  `tctl install trusty-installer` works instead of answering
  `unknown member(s): trusty-installer`. It is a non-daemon and sorts last, so
  a bulk install replaces the running binary only after the rest of the stack
  has landed; both write paths rename atomically over the destination, so the
  running process keeps its open inode (#5805).
- Member names now resolve against every binary a crate installs, read from
  the shared `trusty_common::bin_resolve` table. `tctl install tctl`,
  `tctl install tm`, and `tctl install trusty-embedderd` resolve to
  trusty-installer, trusty-mpm, and trusty-search respectively (#5805).
- `tctl install --dry-run` names the destination directory it would write to —
  the canonical `$CARGO_HOME/bin` (#5777) — and lists every binary each member
  places, not just the one probed for health (#5805).
