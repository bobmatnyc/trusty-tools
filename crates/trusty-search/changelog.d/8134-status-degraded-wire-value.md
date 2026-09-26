Changed

- `GET /indexes/{id}/status` has a third `status` value, `"degraded"`, beside `"indexing"` and `"ready"` (#8134). It is reported when a search stage has failed or a migration fault is outstanding; `stages` and `migration_error` name the cause. A client that treats every value other than `"ready"` as not-ready needs no change.
