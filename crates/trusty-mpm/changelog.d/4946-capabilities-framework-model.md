Added

- `tm-capabilities` gains a generated `references/framework.md` — the install layout, agent tier precedence, skill deploy tiers, per-session state, and an existence-checked index of the authoritative docs (separating what ships in the published crate from what is repo-only). Rendered from the path constants and tier resolvers the runtime uses, so moving a directory or reordering a tier drifts the committed skill and fails `scripts/check_capabilities.sh` (closes [#4946](https://github.com/bobmatnyc/trusty-tools/issues/4946))
  - the compiled prompt's agent-precedence block no longer names `~/.trusty-mpm/agents/`, a tier no code reads; it now states the real order including `$CLAUDE_CONFIG_DIR/agents/`
