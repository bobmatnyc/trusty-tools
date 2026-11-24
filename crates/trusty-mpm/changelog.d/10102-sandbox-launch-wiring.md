Security

- Every launch path now writes and verifies the role's Claude Code sandbox settings before `claude` starts: `tm launch`, `tm connect`, the daemon's managed spawn, resume and in-place relaunch, the Architect launch (`tm fleet init`) and the divert worker. A read-back that does not verify refuses the launch (#10102).
- A launch with no tm-owned Claude config dir (`CLAUDE_CONFIG_DIR` unset) is refused with a message naming `tm doctor`; tm never writes the sandbox's user-level settings into your own `~/.claude` (#10102).
- PM and agent launches in one workspace keep each other's sandbox entries, recorded in `.trusty-mpm/sandbox-roles.json`. The Architect and a sandboxed role cannot share one workspace; the second launch is refused rather than overwriting the first (#10102).
- The Architect's project settings carry the sccache client env (`SCCACHE_SERVER_UDS`, `SCCACHE_CLIENT_SIDE`) when `build.sccache` is on; sandboxed roles keep `RUSTC_WRAPPER` empty (#10102).
- `tm run <alias>` writes and verifies the PM sandbox before `claude` starts, like the other launch paths (#10102).
- A corrupt `.trusty-mpm/sandbox-roles.json` now refuses the launch and names the file, instead of being reset and letting an Architect launch overwrite a recorded PM sandbox (#10102).
- `tm session start` in place writes and verifies the PM sandbox before it registers the session; with no tm-owned config dir it refuses and points at `tm launch` / `tm connect` (#10102).
