Added
- `trusty-memory doctor` reports a palace the daemon has yet to migrate to this binary's format as "needs migration by the daemon" (a warning), apart from a palace it refuses (#9274).
- `doctor` warns that a palace's format could not be verified when its `kg.redb` is held by a running daemon or needs crash repair, instead of reporting it current (#9274).
