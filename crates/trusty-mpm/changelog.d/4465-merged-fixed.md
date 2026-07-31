Fixed

- sessions launched via the `trusty-mpm` binary are now auto-discovered, matching `tm`-launched sessions (closes [#4058](https://github.com/bobmatnyc/trusty-tools/issues/4058))
  - `daemon::discovery`'s tmux-pane predicate only recognised `tm`, so an
    identical session launched under the crate's other `[[bin]]` target
    (`trusty-mpm`) never appeared in `GET /sessions` or other discovery
    lists — no error, just silent invisibility.
  - The crate had four independent hand-copies of "this crate's own binary
    names" (`discovery`'s list, hooks' `MPM_BIN_NAMES`, the statusline
    resolver's `STATUSLINE_BIN_NAMES`, and an inline check in the `tm stop`
    daemon-PID scan) and they had already drifted once. Three now read from
    one canonical `core::own_binary_names::OWN_BINARY_NAMES` constant; hooks'
    list keeps its own array (its PATH-lookup order differs) pinned to the
    same set by a test.
- `session_status` MCP tool no longer reports `delegation_count: 0` for a managed session with subagents genuinely in flight (closes [#4141](https://github.com/bobmatnyc/trusty-tools/issues/4141))
  - The handler resolved a `ManagedSessionId` and passed it straight into
    `delegations_for`, but hook-observed delegations are keyed by the Claude
    session UUID — a different identifier space bridged only via
    `SessionRecord::claude_session_id` /
    `session_start_correlation::correlate_session_start`. The mismatch made
    the surface that answers "what is in flight right now" structurally
    blind to its own feature.
  - The managed branch now reads BOTH key spaces — the managed id
    `agent_delegate` (#1976) writes under, and the bridged Claude UUID
    hook-observed delegations use — and unions them, deduplicated. When the
    bridge itself is missing (no correlated Claude session id yet),
    `delegation_count` and `delegations` both report an explicit `null` with
    `delegation_lookup: "unbridged"` rather than a confident-looking
    `0`/`[]` — an unbridgeable lookup must never be indistinguishable from
    "genuinely none in flight".
