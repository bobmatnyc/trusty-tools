Changed

- The 30 embedded agent `.md` copies that duplicated trusty-mpm's are deleted;
  `assets` now embeds them from `trusty_agents_common::agent_assets`. The
  dispatchable roster and every agent's content are unchanged. The four
  deliberately forked copies (`code-analyzer`, `code-critic`, `qa`, `web-qa`,
  which add a read-only `tools:` restriction) and the four tcode-only defaults
  stay local — those forks are still pinned against the shared source by
  `scripts/check_agent_assets.sh`.
