Added
- New crate `trusty-computer` 0.1.0 with two stub bins, `trusty-computer` and `trusty-computer-mcp`, that print usage and exit 2 (#11460).
- `policy`: the allow list for all 56 cua-driver 0.23.2 tools (6 ALLOW, 13 FILTER, 37 DENY) with the spike's argument checks; `invoke_menu` is denied (#11460).
- `redact`: removes every AXMenuBar element and its markdown subtree from a `get_window_state` reply, returns the element tokens that remain, and fingerprints the redacted tree (#11460).
- `audit`: a fail-closed JSONL log, 0600 and fsynced, rotated at 5 MiB, that records argument keys and text lengths only (#11460).
- `journal`: the per-grant `request_id` journal, capped at 4096 entries, that answers a repeated id without re-running the call (#11460).
- `driver`: the line-JSON client for the cua-driver daemon socket, with per-request timeouts; an `isError` tool result is an error, and the daemon's session-ended refusal is matched on its exact 0.23.2 text (#11460).
- `manifest`: the bounded capability manifest for one lease scope (exact pid and window only), written 0600 (#11460).
- `launcher` and `supervisor`: start, stop, restart and rescope one bounded daemon, launched only through LaunchServices on a private socket under the state dir with telemetry and the update check off. Any CuaDriver other than 0.23.2 is refused, a default daemon socket is refused, and the daemon is identified by socket and metadata pid. A read-only call retries once after its session ends; a mutating call is not re-run (#11460).
- `trusty-computer serve --state DIR` runs the supervised daemon for the scope in `DIR/scope.json`; SIGHUP rescopes and SIGTERM stops it (#11460).
