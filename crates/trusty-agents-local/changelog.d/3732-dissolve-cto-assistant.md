Documentation

- The launcher's module doc no longer describes its missing CTO plugin wiring
  as a temporary state pending a migration. #3732 deleted the crate it would
  have wired in; the CTO DB tools now reach the agent through the declarative
  `cto-db` Python skill, so there is nothing left for this binary to install.
  It remains a thin pass-through to `trusty_agents::run()` — no behaviour
  change.
