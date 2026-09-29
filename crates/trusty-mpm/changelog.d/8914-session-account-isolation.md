Fixed
- `tm sessions new --account X`, `tm sessions start --account X` and `tm launch --account X` now run the session's `gh` and HTTPS git as X. Before, the flag was parsed and dropped, and the session ran as the machine's active gh account (#8914).
- A session pinned to tm's own `~/.trusty-mpm/gh-accounts/<login>` dir now gets that account's token, proven with `GET /user`, plus `GH_CONFIG_DIR` and a git credential helper pin. The dir holds no token, so `GH_CONFIG_DIR` alone resolved the keyring's active account. This also fixes sessions started with `tm <url> --account X` (#8914).
- An `--account` that has no proven credential, an unreadable per-account dir, or a malformed `hosts.yml` or `config.yml` now refuses the command and names the one-time `gh auth login` command. tm never falls back to the active account (#8914).
- `--account` on a `tm sessions` verb that spawns nothing is refused rather than ignored (#8914).
