Added

- An `[accounts]` table in `~/.trusty-mpm/config.toml` maps a GitHub org to the
  `gh` account that clones and spawns for it, for example
  `duettoresearch = "bob-duetto"`. Precedence: `--account`/`--user`/`--u`, then
  the project's registry pin (any pinned field, login from `gh_account` or
  `github.account`), then the table, then the ambient identity. Clone, spawn
  and the launch preflight read the pin through one selection, so they agree. Org names match without case. Only
  github.com remotes are mapped, after resolving an SSH host alias; gitlab.com,
  Bitbucket and Enterprise Server remotes are not.
- A malformed table is an error: `tm run` and `tm launch` refuse and print it,
  a clone refuses without touching the disk, a spawn's `gh` authenticates as
  nobody, and the new `tm doctor` row `org_accounts` reports `Fail`. A TOML
  syntax error in a file where no line names `accounts` (any header or key
  spelling of the table) reads as an empty table with a warning.
