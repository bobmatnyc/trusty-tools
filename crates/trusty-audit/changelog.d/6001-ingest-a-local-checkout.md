Added

- `trusty-audit add repo /path/to/checkout` and a `repos.txt` line naming an
  absolute path now audit a repository that is already on disk, which is the
  only way in when the org it came from is no longer reachable from this
  machine's GitHub credential. An ABSOLUTE path is a checkout and anything else
  is a GitHub `owner/repo` — a syntactic rule, so the same `repos.txt` means the
  same thing on every machine. Acquisition is `git clone <path>`, which hardlinks
  the object store and reads the source only: **the source checkout is never
  modified** — not a fetch, not a checkout, not a stash (a stash list is shared
  across every worktree of a repository, so that is real damage, not a
  theoretical one). The checkout lands under `repos/local/<basename>` and flows
  through the existing sweep to a report and an `extract/<repo>.db`
  indistinguishable from a remotely-cloned one, carrying committed history only.
  A path that does not exist, is not a directory, is not readable, is not a git
  repository, is a subdirectory of one, is a SHALLOW clone, or has no commits is
  refused at registration naming which condition failed — a `--depth=1` source
  would report one commit by one author over a zero-length period (#5916), so it
  is refused rather than audited thinly. A bare mirror is accepted: it carries
  the whole history. Two paths whose basenames match would share one checkout
  directory and are refused at both gates, naming both paths (#6001).
