Fixed
- The `scripts/self-clear.sh` that `tm fleet init` seeds into the Architect now finds the Architect directory from its own path. The `ARCHITECT_ROOT` override is gone, so an environment value can no longer change which scripts it runs, and `tm hook --pm-guard` no longer refuses it because "its path is computed at run time" (#10599).
