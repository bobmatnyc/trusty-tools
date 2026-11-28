Fixed

- `tm launch`, `tm connect`, `tm meta launch` and the Architect launch now refuse a malformed or unreadable `install.toml` before they prepare the session, and the error names the file. Before, the shared session preparation fell back to `$HOME` as the config dir and deployed there. A missing `install.toml` still uses the default (Refs #11821).
