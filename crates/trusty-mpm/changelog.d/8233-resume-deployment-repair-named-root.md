Fixed

- The resume route's deployment repair reads the framework root the daemon
  runs on instead of `$HOME`, so a test driving `resume_managed` no longer
  deploys the agent and skill manifests into the operator's own
  `~/.trusty-tools/trusty-mpm/claude-config/` (#8233).
