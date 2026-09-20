Changed

- The PM instructions' agent-routing table and full-pipeline chain are now
  rendered from the shared `trusty_agents_common::pm_routing` rows instead of
  being authored in `sections/agent-delegation.md`. The delivered text is
  unchanged; a drift test fails if the composed prompt stops matching the
  shared rows (#8293).
