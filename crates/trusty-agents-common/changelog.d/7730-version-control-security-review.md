Changed

- `version-control`'s pre-push credential scan now runs on the agent itself
  and reports the pattern set it checked, instead of delegating to `security`
  — the delegation contradicted BASE-AGENT's "No Subagent Fan-Out" rule when
  `version-control` ran as a dispatched subagent. A high-risk branch gets
  `security` dispatched by the PM before `version-control` starts, not
  mid-task
  (refs [#7730](https://github.com/bobmatnyc/trusty-tools/issues/7730))
