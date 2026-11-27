Fixed
- `tm daemon --sandbox` refuses to start when the sandbox home's `.trusty-tools/install.toml` does not resolve, or names a config dir whose `.trusty-tools` or `.trusty-mpm` is the account's own, so a sandbox daemon never writes the real config dir (#11821).
