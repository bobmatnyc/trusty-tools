Added

- An `[accounts]` table in `~/.trusty-mpm/config.toml` maps a GitHub org to the
  `gh` account that clones and spawns for it, for example
  `duettoresearch = "bob-duetto"`. It applies when no `--account`, `--user` or
  `--u` is given and no registry pin names an account; an explicit selection
  always wins, and an unmapped org behaves as before. Org names match without
  case. A malformed table is an error: a clone refuses, and a spawn's `gh`
  authenticates as nobody, rather than running as the machine's active account.
