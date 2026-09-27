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
