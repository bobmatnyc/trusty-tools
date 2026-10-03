Fixed

- `tm pr cleanup` states how each kept worktree's tip relates to the merged
  head: it is the head, it is an ancestor (every commit on it is in the
  merge), or it carries N commits the merge did not, which gets a separate
  WARNING line. An unreadable relation is stated as unknown.
