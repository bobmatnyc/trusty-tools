Changed

- Azure DevOps `work_items` rows are now keyed `AB#42` rather than a bare `42` (#5219). `collect::correlate_commits` looks a commit's ticket key up against `work_items.id`, and `collect::ticket::extract_ticket_id` yields `AB#42` for an ADO reference — so no ADO row a previous run wrote could be matched by the correlation pass. The shared writer stores the adapter's canonical id, which is the form correlation searches for.

  The correlation pass was never the only writer, though: the old ADO writer inserted its own `commit_work_items` links directly, under the same bare numeric key. So an existing database holds legacy rows AND legacy links, and the next collect writes the `AB#42` rows beside them rather than replacing them. Delete the children first — `commit_work_items` references `work_items(id, source)` with no `ON DELETE CASCADE`, and every connection runs `PRAGMA foreign_keys = ON`, so removing the parent rows alone fails with `FOREIGN KEY constraint failed`:

  ```sql
  DELETE FROM commit_work_items WHERE work_item_source = 'azdo' AND work_item_id NOT LIKE 'AB#%';
  DELETE FROM work_items        WHERE source = 'azdo' AND id NOT LIKE 'AB#%';
  ```

  Both statements are safe to re-run, and neither touches a correctly-keyed `AB#` row or any other source.

  `PmTicket` gains a `project` field, carrying what ADO reports as `System.TeamProject` into `work_items.project` — the column DOC-70's board axis filters on. The struct is now `#[non_exhaustive]` so the next field is not another break. `PmSource::work_item_source` is new: it returns the database vocabulary, which is `azdo` for Azure DevOps where `PmSource::as_str` returns the display label `azure_devops`.
