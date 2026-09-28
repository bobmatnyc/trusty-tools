Fixed
- `discovers_trusty_git_analytics_alias` and `dispatch_discover_aliases_inserts_new_and_dedupes` build their `tga` → `trusty-git-analytics` workspace in a tempdir instead of reading `crates/trusty-git-analytics`, which left this workspace (#8824). Test-only.
