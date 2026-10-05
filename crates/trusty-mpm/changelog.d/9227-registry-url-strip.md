Fixed
- `tm load` and `tm run` strip the token from a registry URL stored before #9124 before they clone, so the new clone's `.git/config` and `.trusty-mpm/managed.toml` hold no credential; `registry.json` is left as stored. A URL whose credential cannot be stripped, such as a password holding a raw `/`, is refused before any clone, and the error redacts it (#9227).
- Clones made before this fix keep their stored URL; run `git remote set-url origin <clean-url>` and re-register to clean them.
