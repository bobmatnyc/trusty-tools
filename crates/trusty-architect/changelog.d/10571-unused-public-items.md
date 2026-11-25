Removed

- Unused public items with no caller in the workspace: `sched::parity::Report::match_pct` (the verdict uses `alert_pct` and `wake_pct` since #11622), `il::ops::is_live`, `dashboard::pending::Pending::into_events`, the `dashboard::schema_check::Pattern` re-export, and `il::store::LockGuard::raw_fd`. Each is a public API break against 0.4.2, declared in `scripts/semver-accepted-breaks/trusty-architect-0.4.3.txt` (Refs [#10571](https://github.com/bobmatnyc/trusty-tools/issues/10571))
- The four copies of the civil-date helpers are now one `days_from_civil` and one `civil_from_days` in `classify::alerts`; results are unchanged, and a table test checks them against the removed copies
