Fixed

- The agent roster is resolved by exactly one function, so the count `tm session
  start` prints can no longer disagree with the roster the PM receives. The
  printed number came from a single-directory scan of the tm-managed deploy tier
  while the delegation section delivered to the PM was a three-tier union
  (project `.claude/agents` + `$CLAUDE_CONFIG_DIR/agents` + the operator's
  `~/.claude/agents`) — two independent implementations of the same question,
  measured live at 37 printed against 42 delivered. `build_instructions` now
  takes the project rather than an agents directory and resolves the roster
  through `delegation_authority::resolve_roster`, which the PM prompt and
  `tm doctor` also call
  ([#4588](https://github.com/bobmatnyc/trusty-tools/issues/4588)).
- Agents are no longer excluded from the delegation roster because their
  frontmatter `role:` begins with `base`. Foundation templates are identified by
  the `BASE-*` file-name convention alone. The frontmatter rule silently deleted
  three real, deployed, dispatchable agents — `memory-manager`,
  `mpm-agent-manager`, `mpm-skills-manager` — from every roster the PM ever saw
  (34 advertised where 37 were deployed), and it could not be fixed by editing
  assets: the roster unions two tiers whose frontmatter tm does not author, so
  the same rule would eat an operator's own agent with no recourse
  ([#4589](https://github.com/bobmatnyc/trusty-tools/issues/4589)).
- `tm doctor`'s `agents` check reports the delegatable roster size alongside the
  deployed file count, resolved by that same function, and reports it as
  `unknown` rather than guessing when no project directory scopes the probe. A
  file count is a deploy fact, not a routing fact; reporting only the former is
  how a 42-file, 34-agent install read as healthy
  ([#4589](https://github.com/bobmatnyc/trusty-tools/issues/4589)).
