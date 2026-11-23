Added

- The desktop client now opens on an **assistant picker** instead of dropping
  straight into a conversation (closes
  [#4404](https://github.com/bobmatnyc/trusty-tools/issues/4404)). The landing
  view draws a card per assistant INSTANCE (Izzie, the CTO Assistant, anything
  you created), a card for the Concierge, and a "New assistant" action that
  reuses the existing `POST /api/agents` template flow rather than duplicating
  it. An "Assistants" tab keeps the picker reachable without a relaunch.
  - Selection sticks by itself: it writes `activeAgentId`, which
    [#4281](https://github.com/bobmatnyc/trusty-tools/issues/4281) already made
    the persistence surface. There is no picker-owned storage key and no save
    call.
  - The Concierge card carries the id `ctrl` (matching the persisted sentinel
    and the config surface) and is decoded back to `null` before it reaches the
    dispatch axis. Dispatching `ctrl` BY NAME routes through the tools-OFF
    persona path and would silently strip Concierge's delegation capability, so
    the decode is a correctness guard, not a formality. `ctrl` is also excluded
    from the roster-driven rows, so Concierge appears exactly once.
  - Card art is deliberately NOT generated. Per the owner's decision, upload is
    manual for now and generation is a fast-follow entangled with
    [#4405](https://github.com/bobmatnyc/trusty-tools/issues/4405)'s undecided
    model choice; cards carry a deterministic monogram tile as a typographic
    stand-in, adding no dependency.
  - `ProjectsView` deliberately gains no nav entry — it stays unrouted per
    [#3819](https://github.com/bobmatnyc/trusty-tools/issues/3819), with epic
    [#4355](https://github.com/bobmatnyc/trusty-tools/issues/4355) rebuilding
    that surface.

- The Sub-agents configuration pane now surfaces the **tcode coding-delegation
  target and an execution-style selector** (closes
  [#4353](https://github.com/bobmatnyc/trusty-tools/issues/4353); spec DOC-62).
  `coding-pm` — the sole coding delegation surface, made addressable by
  [#4350](https://github.com/bobmatnyc/trusty-tools/issues/4350) — is shown as a
  named third mechanism alongside `delegate_to_agent` and the non-coding
  `dispatch_task` targets, never folded into them: its reserved name is
  recognised before the non-coding allow-set is consulted, so
  `[subagents].allowed` does not gate it, and the pane says so positively rather
  than leaving it to be inferred.
  - The selector renders the **effective** style and its resolution path, not
    the requested one (DOC-62 §3.4, OQ-6). A style is a ceiling request the
    callee may raise and never lower, and SM-9 raises `vibe` to `engineer` for
    as long as the VIBE tier is unimplemented
    ([#2596](https://github.com/bobmatnyc/trusty-tools/issues/2596)) — so today
    every request to this lane resolves to `engineer`, and the pane states the
    requested value, the effective value, the precedence level it came from, and
    each escalation reason in plain language.
  - Every resolution is computed server-side by the real
    `ResolvedStyle::resolve` over the real lane floor and the agent's real
    `[subagents] default_style`, and served on
    `GET /api/agents/:name/subagents`. Nothing is re-derived client-side, so the
    pane cannot disagree with the bridge and will not go stale when #2596 lands.
  - The `engineer` STYLE is labelled to distinguish it from the tcode
    `engineer` SUB-AGENT role, per DOC-62 OQ-3's recommended mitigation. OQ-3 is
    not formally ratified.
  - Read-only, like every sibling section: `[subagents] default_style` has no
    write route, and DOC-62 OQ-1 (per-assistant vs global default scope) is
    still open, so the pane reports the configured default and points at
    `agent.toml` rather than persisting a choice against an unratified decision.
