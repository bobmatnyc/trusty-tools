Fixed

- The OSV scan no longer sends an unresolved locked cell to OSV.dev as if it
  were a version. `trusty-review` writes `0.13.1, 0.22.1 (none satisfies ^0.30)`
  into `locked` when no locked version satisfies the declared requirement, and
  that string was queried verbatim, asking about a package that does not exist.
  The row is now named in the scan's unpinned gap line instead, using the
  `resolved` flag the producer records (issue #6794). A snapshot written before
  that flag existed carries no such key and behaves exactly as before.
