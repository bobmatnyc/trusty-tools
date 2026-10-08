Fixed
- A normal stop (`trusty-search stop`, SIGTERM, admin stop) now closes every redb corpus before the daemon exits, so the next start no longer repairs each one and a read-only open of a stopped daemon's corpus succeeds instead of failing with `RepairAborted` (#9459).
- `search.project.resolve` keeps reading `reindexed_unix` after a graceful restart; it fell back to the corpus mtime because the read-only stamp read failed (#9477).
- The close waits at most 3 s. A corpus still held at that deadline is named in a `warn` log line and repaired on the next open, as before.
- An index whose reindex, relocate or embed pass is still running at the stop keeps its corpus attached, is named in the same `warn` line, and is repaired on the next open. Taking that corpus would let the reindex write its HEAD marker with its last batches unsaved.
