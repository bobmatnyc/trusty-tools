Fixed

- `tm-architect-setup` no longer names a `tm fleet add` command, which does not exist; it documents the two `[notification_hook]` keys in `~/.trusty-mpm/config.toml`, that `tm fleet init` writes them, and `tm doctor --fix --only notification_hook --yes` as the step after editing them by hand (#10885).
