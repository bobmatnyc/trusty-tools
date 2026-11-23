Changed
- `launchd_labels::AGENTS_SLACK` (`com.trusty.agents.slack`) moves to `RETIRED_SERVICES` under member `trusty-assistants`: `tagent --slack` no longer exists, so nothing installs the unit and an upgrade evicts it (#11632).
- `bin_resolve` maps `trusty-assistants` to its `ta` binary and the one-release `tagent` alias, replacing the `trusty-agents` row (#11632).
