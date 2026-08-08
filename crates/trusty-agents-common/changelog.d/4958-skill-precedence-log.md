Fixed

- the skill tier-collision log no longer claims a winner it cannot deliver. It said "deploying the higher-precedence copy", which for a project-custom winner stated the opposite of what happens — Claude Code resolves skills personal over project, so a same-named copy under `$CLAUDE_CONFIG_DIR/skills` still beats the project-tier file. The line now scopes itself to deploy-time source precedence within one destination and names that `dest`
