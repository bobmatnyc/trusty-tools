Added

- The `tm ls` picker's delete accepts a name pattern: `d tm-test-*` deletes
  every matching session in one action instead of one `d<N>` per session. The
  pattern is a glob (`*`, `?`, `[…]`, case-insensitive) matched against the tmux
  session name, and `d <glob> --dry-run` previews without deleting. Deletions
  route through the same managed→local path `d<N>` and `tm session delete`
  already use. Four guards bound the blast radius: only stopped/errored sessions
  are ever deleted in bulk (a running match is listed as kept, and force is
  never available in bulk); the session you are running inside is excluded by
  tmux name even if its record reads stopped (best-effort — the driver warns
  when that probe fails rather than implying a guard it did not apply); a
  session with no tmux name matches no pattern at all, `*` included; and
  confirmation is the number of `DELETE` rows typed back, not `y`, after the
  full match set is printed. The prompt deliberately does not print that number
  — printing it would let the batch be confirmed without reading a row. A
  pattern matching nothing says so and exits without deleting, and a per-session
  failure mid-batch is reported and tallied instead of aborting the run.
  Documented for agents in the bundled `tm-cli-operations` skill.
