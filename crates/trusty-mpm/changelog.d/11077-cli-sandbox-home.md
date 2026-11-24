Fixed
- A sandboxed `tm launch` / `tm connect` (`TRUSTY_SANDBOX=1`) now writes its session-MCP config and launch settings under the sandbox home, and refuses when the sandbox data dir cannot be resolved, instead of writing under the real home (#11077).
