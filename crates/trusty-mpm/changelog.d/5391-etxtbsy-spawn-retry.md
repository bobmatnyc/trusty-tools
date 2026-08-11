Fixed

- `core::spawn_disclaim::disclaimed_output` retries a spawn that fails with
  `ETXTBSY` ("Text file busy") up to three times with exponential back-off,
  instead of surfacing it as a hard error. `ETXTBSY` is raised while any
  process holds a writable fd to the target inode, so a sibling thread that
  forks between this process's write and close of a freshly created
  executable can make an otherwise valid exec fail — the race that made the
  `tmux` fake-binary tests flaky in CI. A permanently busy target still fails
  after the third attempt (#5391, epic #3451).
