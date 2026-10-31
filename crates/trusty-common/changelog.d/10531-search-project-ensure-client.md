Added
- `search_rpc::METHOD_PROJECT_ENSURE`, `ensure_project_params` and `ensure_project_blocking` call the daemon's `search.project.ensure` (#10531).
- `search_index::best_effort_trigger_reindex` is public, so a caller that found an index through `search.project.ensure` can run the freshness-gated reindex trigger (#10531).
