Fixed
- With a broken `install.toml`, `--account-token-stdin` (`tm sessions new`, `tm launch`, `tm <url>`) refuses with an error naming the file before it reads the token, instead of storing it under `$HOME/.trusty-mpm/gh-accounts` (#11821).
