Fixed
- The `stale-tasks` port reads each row's fields from the last document with that id, as the frozen `stale_ranked` does, where two documents share an id; its rules and the jsonl line parser now come from the `dashboard_derive` and `dashboard_jsonl` ports (#12029).
