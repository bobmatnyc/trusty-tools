Fixed

- **The agent catalog and by-name dispatch no longer disagree about a
  trusty-mpm agent's role** (closes
  [#4511](https://github.com/bobmatnyc/trusty-tools/issues/4511)). Two code
  paths read the same on-disk artifact — trusty-mpm's deployed
  `.claude/agents/*.md` files — and derived different roles from it. The
  by-name DISPATCH path (`agents::claude_mpm_loader`) passes BOTH dialect keys
  to `agents::claude_mpm_role::normalize_role` since
  [#4506](https://github.com/bobmatnyc/trusty-tools/issues/4506); the
  registry/CATALOG path (`agents::mpm_bridge`) passed only `role:` and so
  resolved every claude-mpm-format artifact — the ones that declare
  `agent_type:` and no `role:` at all, which is most of them — to the
  fail-closed sentinel `"agent"`. `mpm_bridge` now hands both keys to the same
  shared function, so one file on disk yields one role
  - The mapping is NOT duplicated. `normalize_role` remains the single
    reviewed table; the preference order (`role:` wins, `agent_type:` is the
    fallback), the trim/case-folding, and the fail-closed default all stay
    there. `catalog_and_dispatch_derive_the_same_role` pins the two paths
    together over seven artifact shapes, so either side silently dropping the
    shared derivation now fails the suite
  - `agent_type:` moved from the "dropped, had no effect" warning into
    `CONSUMED_KEYS`, because it is now genuinely read
  - Still fail-closed and still deliberately unmapped: `security`,
    `version-control`, `code-analyzer`, `memory-manager`, `mpm-agent-manager`,
    `mpm-skills-manager`, every `base-*` composition fragment, and
    `universal`/`system`/`trusty-mpm`. Admitting any of them remains an owner
    capability decision, not a translation — `version-control` in particular
    is still not delegable
  - NO capability change, PROVEN rather than asserted.
    `normalized_mpm_role_still_reaches_nothing` takes an artifact that this
    change makes role-ELIGIBLE (`agent_type: ops` → `ops`, a member of
    `ASSISTANT_ALLOWED_DELEGATE_ROLES`), hands it to a `delegate_to_agent`
    wired exactly as `build_assistant_tier_registry` wires it — L0 assistant
    delegator, the real role allowlist, and the ENTIRE server-owned
    `ASSISTANT_REACHABLE_SUBAGENTS` floor granted, the most permissive posture
    any assistant can hold — and the delegation is still refused with the
    runner never reached, while the same tool in the same test reaches a real
    floor name (so the refusal is not a suite that denies everything).
    `an_mpm_artifact_cannot_shadow_a_floor_name` covers the other half: a
    bundled `<name>.toml` wins name resolution ahead of any `.md`, so an mpm
    artifact cannot occupy a floor name and inherit its reachability.
    `ASSISTANT_REACHABLE_SUBAGENTS`, `[subagents].delegate_allowed`, and the
    delegate tool's allow-set are untouched
