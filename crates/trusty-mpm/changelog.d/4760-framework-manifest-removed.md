Removed

- the deprecated `ops` agent is deleted from the bundle; it was superseded by
  `local-ops` and still reached every roster
  ([#4760](https://github.com/bobmatnyc/trusty-tools/issues/4760))
  - an `ops.md` already deployed to a machine is NOT retracted — orphan
    retraction is [#391](https://github.com/bobmatnyc/trusty-tools/issues/391)
    and has not shipped
  - trusty-code's embedded mirror of the agent catalog drops `ops.md` and gains
    `elixir-engineer.md`, keeping `scripts/check_agent_assets.sh` green
  - the bundled `tm` skill and `agent-delegation.md` no longer name `ops` in
    their rosters, and now say which agents are marker-gated rather than
    implying every bundled agent reaches every project
