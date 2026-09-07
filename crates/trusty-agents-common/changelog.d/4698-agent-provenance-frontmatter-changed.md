Changed

- `agents::builder::AgentBuildError` gained an `InvalidProvenance` variant. An
  unrecognised `provenance:` value is rejected rather than degraded to the absent
  default, which would read a typo as user-authored and freeze a framework file
  permanently — #4408's unrecoverable shape reached through a new door (#4698).
