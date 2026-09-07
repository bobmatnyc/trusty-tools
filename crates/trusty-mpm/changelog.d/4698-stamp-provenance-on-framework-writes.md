Fixed

- `update_check::compute_agent_catalog_hashes` and `agent_reset::reset_agents`
  now compose with `compose_agent_with_provenance(…, FrameworkOwned)`, the same
  call the agent deployer makes. Both hash or write what the deployer writes;
  composing unstamped would have differed from every deployed file by the
  `provenance:` line alone and reported the whole roster permanently stale
  (#4698).
