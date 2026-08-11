Fixed

- `search_index::ensure_project_indexed` and `ensure_project_indexed_with` no
  longer return an index id when the daemon never confirmed the index
  ([#5091](https://github.com/bobmatnyc/trusty-tools/issues/5091)). A refused
  `POST /indexes`, a transport error, an undiscoverable daemon, or the #4255
  test-harness suppression all yield `None` and a `warn` line, so a caller
  cannot pin an index that does not exist. `ensure_project_indexed_reporting`
  is unchanged and still carries the derived id in every case, beside the
  `IndexRegistration` that says what the daemon actually did. Errors are still
  never propagated — a search outage never blocks a session launch or a task
  run.
