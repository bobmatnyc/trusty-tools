Fixed
- `tm launch`, `tm connect`, `tm fleet init`, `tm meta launch` and the daemon's in-project session start now write the user-tier statusLine, trust seed and legacy-hook cleanup into the `CLAUDE_CONFIG_DIR` directory when it is set, not into `~/.claude` ([#10025](https://github.com/bobmatnyc/trusty-tools/issues/10025))
- A sandboxed `tm session start` (`TRUSTY_SANDBOX=1`) no longer writes the trust seed, statusLine seed or legacy-hook cleanup into the caller's `CLAUDE_CONFIG_DIR`; it keeps them under the sandbox home ([#10025](https://github.com/bobmatnyc/trusty-tools/issues/10025))
