Added
- `tm fleet init --session <name>` and `tm fleet status --session <name>` set the Architect's tmux session name; the poller runs as `<name>-poll`. `init` records the name as `[supervisor] session` in `~/.trusty-mpm/config.toml` and beside the launch record, so later runs without the flag use the same name. Without the flag the names stay `tm-architect` and `tm-architect-poll`. Invalid names are refused before any write (#8878).
- `tm fleet status` reports `binding`: whether the `claude` in the Architect's session is the one `tm fleet init` launched under that session name (#8878).
- The `tm fleet init` preflight accepts one private child repository at `<dir>/local` when it has no git remote and holds no nested repository; every other child repository is still refused (#8878).
