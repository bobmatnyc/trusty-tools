Added
- `search_rpc::METHOD_PROJECT_HEALTH`, the `ProjectHealthCounts` reply type and the `project_health_at` client for trusty-search's new `search.project.health` counts (#10535). A reply without all five counts is an error, never zero counts.
