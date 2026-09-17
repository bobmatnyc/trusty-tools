Added
- `tcode tui` shows a startup splash on connect: robot header, the binary's version and git SHA, the daemon's version and build SHA, the session id, the project binding (or "projectless"), the active workstream, and the session's delegation shape. A client/daemon build mismatch prints one plain warning naming both. The splash replaces the banner's old `tcode v0.2.0` sub-crate header. (#8164)
- The `health` RPC and `GET /health` now report the daemon's build SHA in a `build` field, so a client can tell a stale daemon from a fresh one. (#8164)
