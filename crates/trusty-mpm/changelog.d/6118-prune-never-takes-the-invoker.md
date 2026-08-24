Fixed

- No prune path can select the session it was invoked from. The CLI now sends
  `$TM_MANAGED_SESSION_ID` as `invoking_session`, and the prune engine excludes
  that record for every filter, `--include-active` included. A present but
  unparseable value is rejected with 400 rather than dropped, so the sweep never
  runs with the self-exclusion silently switched off.
