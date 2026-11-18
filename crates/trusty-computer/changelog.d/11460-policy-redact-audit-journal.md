Added
- New crate `trusty-computer` 0.1.0 with two stub bins, `trusty-computer` and `trusty-computer-mcp`, that print usage and exit 2 (#11460).
- `policy`: the allow list for all 56 cua-driver 0.23.2 tools (6 ALLOW, 13 FILTER, 37 DENY) with the spike's argument checks; `invoke_menu` is denied (#11460).
- `redact`: removes every AXMenuBar element and its markdown subtree from a `get_window_state` reply, returns the element tokens that remain, and fingerprints the redacted tree (#11460).
- `audit`: a fail-closed JSONL log, 0600 and fsynced, rotated at 5 MiB, that records argument keys and text lengths only (#11460).
- `journal`: the per-grant `request_id` journal, capped at 4096 entries, that answers a repeated id without re-running the call (#11460).
