Fixed

- Registering or indexing a repo no longer edits the repo's `.gitignore`. A
  new index now keeps its store in the data dir, outside the work tree, so
  `git reset --hard` followed by `git clean -fdx` no longer deletes a live
  index. A repo that already holds a `.trusty-search/` index keeps it. That
  directory now hides itself from `git status` and `git clean -fd` with its
  own `.gitignore` (`*`). Registration refuses with `409` when the store would
  land inside the repository. An earlier uncommitted `.gitignore` edit is left
  alone; revert it with `git checkout -- .gitignore`. To move an existing
  in-repo index out of the work tree, delete it with `delete_data=true` and
  register it again ([#8499](https://github.com/bobmatnyc/trusty-tools/issues/8499))
- Registration also refuses with `409` when the data dir itself sits inside
  the index root, for example `TRUSTY_DATA_DIR` set to a path in the repo, or
  a dotfiles repo at `$HOME` over the default data dir. Before, the whole
  store was written into the work tree in that case. `PATCH /indexes/:id`
  applies the same rule to the new root
  ([#8499](https://github.com/bobmatnyc/trusty-tools/issues/8499))
- `POST /indexes` no longer waits on every other registration in the daemon.
  Only registrations under the same id, or over the same or a nested root,
  wait for each other ([#8499](https://github.com/bobmatnyc/trusty-tools/issues/8499))
- `PATCH /indexes/:id` (relocate) answers `409` while a reindex,
  deferred-embed pass or component catch-up runs on that index, instead of
  moving the root under the running walk. Retry once it finishes
  ([#8499](https://github.com/bobmatnyc/trusty-tools/issues/8499))
