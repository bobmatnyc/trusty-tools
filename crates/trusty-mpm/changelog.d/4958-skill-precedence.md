Documentation

- `mpm-skills-manager` and `mpm-agent-manager` now carry Claude Code's verified runtime resolution order for their own artifact type, and state the asymmetry between them: for skills personal `~/.claude/skills/` beats project, for agents project `.claude/agents/` beats user. Each says where to write a file so it actually wins the collision
  - the skills manager's existing tier section is relabelled deploy-time source precedence, so tm's `project-custom > user-custom > bundled` rule is no longer read as Claude Code's runtime order
