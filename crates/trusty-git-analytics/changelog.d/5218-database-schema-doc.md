Documentation
- `docs/trusty-git-analytics/requirements/database-schema.md` is rebuilt from
  the migrations. The migration-history table stopped at 0013 and now runs
  through 0027, one row per migration naming its file; two of the rows it did
  have were wrong (`0012` is `pull_requests_repository`, not
  `repository_analysis_status`, and `0013` puts `complexity` on
  `classifications`, not on `commits`). Thirteen tables and all four DORA views
  had no section at all. Eight existing sections described columns the shipped
  schema does not have — `work_items` still listed the `provider` /
  `external_id` / `work_item_type` / `state` shape migration 0005 never used,
  `linear_issues` named `issue_id` instead of `identifier`, `commit_work_items`
  and `classification_overrides` described integer keys where both use composite
  text ones, `repository_analysis_status` named four columns it does not have,
  and `pull_requests` listed a `merge_commit_sha` that has never existed.
