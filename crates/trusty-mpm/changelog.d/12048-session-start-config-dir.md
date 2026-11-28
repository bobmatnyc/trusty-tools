Fixed

- `tm session start` and the TUI launch now deploy the framework agents and skills under the configured `config_dir` (`install.toml`), the root `tm launch` reads, instead of `$HOME/.trusty-mpm/framework/` (Refs #12048, #11821).
- `tm session start` and the TUI launch now refuse a malformed `install.toml` before they deploy anything, instead of deploying under `$HOME`. A missing `install.toml` still uses the default (Refs #12048, #11821).
