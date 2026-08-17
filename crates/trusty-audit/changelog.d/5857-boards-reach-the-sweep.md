Added

- A registered board is now collected instead of reported as a gap. `jira:ACME`
  or `linear:ENG` becomes a `jira:` / `linear:` section on each generated tga
  config, so `tga audit` syncs the board alongside the repositories. Registering
  a board previously stated it as a gap and held the whole engagement at a
  non-zero exit.
