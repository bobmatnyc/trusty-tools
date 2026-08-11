Added

- The `tm ls` picker's delete accepts a name pattern: `d tm-test-*` deletes
  every matching session in one action instead of one `d<N>` per session. The
  pattern is a glob (`*`, `?`, `[…]`, case-insensitive) matched against the tmux
  session name, and `d <glob> --dry-run` previews without deleting. Deletions
  route through the same managed→local path `d<N>` and `tm session delete`
  already use. Four guards bound the blast radius: only stopped/errored sessions
  are ever deleted in bulk (a running match is listed as kept, and force is
  never available in bulk); the session you are running inside is excluded by
  tmux name even if its record reads stopped; a session with no tmux name
  matches no pattern at all, `*` included; and confirmation is the match count
  typed back, not `y`, after the full match set is printed. A pattern matching
  nothing says so and exits without deleting.
