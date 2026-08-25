Added

- `trusty-audit init` writes the first `engagement.toml` with no terminal
  involved, reading the key from `OPENROUTER_API_KEY`. Only the interactive
  cold start ever wrote that file, so the README's documented sequence failed at
  `add repo` with `NoEngagementConfig` for every scripted and CI caller, whose
  workaround was faking a pty with `script -q /dev/null`. Re-running it over an
  existing engagement is a no-op, so it can sit at the top of a script.
