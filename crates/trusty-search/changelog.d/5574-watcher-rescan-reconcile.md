Fixed
- The file watcher no longer silently drops the OS "you missed some, re-scan"
  signal. On a kernel or user event-queue overflow `notify` raises
  `Flag::Rescan`, but `notify-debouncer-mini`'s `DebouncedEvent` has no field
  for the flag, so it was destroyed before the watcher saw it — on macOS the
  surviving path names a directory, which `handle_modified` discarded at its
  `is_dir()` guard, and on Linux the event carries no path at all. The daemon
  never learned about any change in the dropped batch, so searches over those
  files answered as though the edits had never happened.
- Raw events are now tapped ahead of the debouncer, and a rescan triggers a full
  re-walk of the watched tree: every walked file is re-indexed in bounded
  batches, chunks for tracked files that no longer exist on disk are dropped,
  and the symbol graph is rebuilt once.
- A reconcile that does not fully reconcile the tree is retried with backoff
  instead of being reported as success. That covers both ways a pass falls
  short: it returns an error, or it returns successfully having skipped files it
  could not read. The partial case previously cleared the consecutive-failure
  count and scheduled nothing, so a transiently unreadable file was left stale
  behind a single WARN until some unrelated event happened to touch it. Files
  that could not be read are counted into `RescanStats::files_unreadable`.
