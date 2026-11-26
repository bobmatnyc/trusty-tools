Fixed

- Watcher tests that start a real OS file watcher retry a start that overruns the 5 s `WATCHER_START_BOUND` on a loaded host, instead of failing on host timing (#9466). Test code only; the production bound and watcher behaviour are unchanged.
