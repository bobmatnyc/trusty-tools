Fixed
- The Architect scripts `tm fleet init` seeds take the projects root and the claude-config dir from the install's paths (`TRUSTY_MPM_WORKSPACE_ROOT` / `TRUSTY_MPM_CONFIG_DIR`, then `~/.trusty-tools/install.toml`, then the default) instead of fixed home paths, and a broken install.toml is an error rather than a fall back to home (#11821).
