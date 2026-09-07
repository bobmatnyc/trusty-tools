Changed

- An MCP read tool called with no `index_id` on an unpinned session now returns the registered indexes plus a retry hint as a successful result, instead of erroring. Applies to `search`, `search_lexical`, `search_semantic`, `search_kg`, `typeahead`, and `index_status`; `grep` and `search_all` already fanned out. `index_id` is no longer `required` in those tools' schemas. The mutating tools — `index_file`, `remove_file`, `delete_index`, `reindex` — keep the error (#6317).
