Added

- `tm session prune --state unresolvable` selects managed records that name no
  resolvable workspace — `cwd` is the `/unknown` sentinel and there is no
  `workspace_path`
  (refs [#6118](https://github.com/bobmatnyc/trusty-tools/issues/6118)).
  These are the adoption ghosts #6126 stopped minting; nothing could select the
  ones already in the store. On the reporting host 23 of them sat `Active`:
  every existing `--state` value selected 0, `prune-idle` skipped all 23 on an
  `errored` verdict, and the only command that reached them — `--state all
  --include-active` — also selected 32 healthy working sessions including the
  operator's own. The new filter is the one selector whose choice ignores tmux
  liveness, because a live pane is what made the class unreachable; it needs no
  `--include-active` and is not widened by it. Such a record has no
  `workspace_path`, so this path can delete no file: it reclaims the leaked pane
  and the store row. Two records sharing one pane (the #6117 race) are both
  tombstoned in one pass.
- `tm session decommission-ephemeral --dry-run` previews the sweep instead of
  performing it. Preview and real sweep are one call differing in one boolean, so
  they cannot select different sets. The daemon echoes `dry_run` and the CLI
  refuses to report a preview the daemon did not confirm — a daemon predating
  this change drops the unknown query param and sweeps for real while still
  returning 200.
