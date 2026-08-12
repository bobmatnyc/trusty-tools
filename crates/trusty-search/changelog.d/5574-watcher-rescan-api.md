Added
- `WatchEvent::Rescan`, a new variant on the public `WatchEvent` enum, carrying
  the dropped-event signal to the watch loop. Breaking for exhaustive downstream
  matches, so this needs the 0.x minor position rather than a patch bump.
- `IndexedFiles::paths`, used by the rescan reconcile to find files deleted
  while the event queue was overflowing.
