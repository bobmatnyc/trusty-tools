Added

- A regression test for each of the five attendance entry points, asserting
  through a tempdir root — the tests that could not be written before. Each was
  confirmed to fail when its handler's hook is removed.
- `attendance::AttendanceRoot`, the injected-root shape the chat transports
  thread through their handlers.
