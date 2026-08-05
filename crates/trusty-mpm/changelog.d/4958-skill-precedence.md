Documentation

- corrected trusty-mpm's stated skill precedence, which had Claude Code's runtime order backwards and conflated it with tm's deploy-time order
  - `mpm-skills-manager` and `mpm-agent-manager` now carry the verified runtime resolution for their own artifact type and state the asymmetry: for skills personal `~/.claude/skills/` beats project, for agents project `.claude/agents/` beats user. Each says where to write a file so it actually wins the collision
  - the skills manager's existing tier section is relabelled deploy-time source precedence, so tm's `project-custom > user-custom > bundled` rule is no longer read as Claude Code's runtime order
  - `core::project_skill_tier` and `core::managed_config` asserted that `<project>/.claude/skills` outranks `$CLAUDE_CONFIG_DIR/skills`. That is backwards for skills, and it was load-bearing for the #4880 redeploy rationale. Both now state the real order and name which axis they mean; the redeploy still earns its place because the project tier is what loads for every name the managed roster does not carry
