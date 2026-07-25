<script lang="ts">
  /**
   * Why (#3932, DOC-57 §7 / §8.2): section 5 of the five-section agent config
   * — "what the agent is allowed to do, and on whose authority". This is the
   * section the spec CREATES rather than relabels: an agent's permission
   * posture is spread across four unrelated mechanisms in three locations with
   * three different failure polarities (absent-denies, empty-denies,
   * default-opens), two of which are invisible in config entirely. A chip list
   * of scope strings, which is what this tab used to be, shows none of that.
   *
   * What: read-only rows over the config the sidecar already serves, each
   * carrying an explicit ENFORCED / NOT ENFORCED marker. PM-6 makes that
   * marker mandatory — displaying an unenforced control as if it constrained
   * the agent is the permissions-surface equivalent of a fabricated listener
   * pane — and PM-4 makes read-only permanent here, in every phase: granting
   * capability from a GUI is a security-relevant write path needing its own
   * review, and no route in DOC-57 adds one (C-06.4).
   *
   * The `enforced` values are stated from §7.1's mechanism table, with its
   * qualifiers intact rather than rounded up to a green tick — scope
   * enforcement is real but path-limited, and `user_authority` does not exist
   * as a field at all yet (#3074). The pane also names its own blind spot:
   * `parse_agent_toml` (`projects.rs:369`) reads ONE file and performs no
   * `extends` merge, so what is shown is what this agent DECLARES, not its
   * resolved effective set. PM-7's `source: declared | inherited:<base>`
   * annotation — the thing that would make §7.3's cto-assistant defect visible
   * — needs `GET /api/agents/:name/permissions` (§7.4, Phase 4).
   * Test: `AgentConfigPermissions.test.ts`.
   */

  /** `[tools].allow` as declared in this agent's own `agent.toml`. */
  export let toolsAllow: string[] = [];
  /** `[tools].scopes` as declared in this agent's own `agent.toml`. */
  export let scopes: string[] = [];

  const heading =
    'shrink-0 font-mono text-[10px] font-semibold uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50';
  const enforcedChip =
    'shrink-0 rounded-md bg-foundry-teal/15 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-foundry-teal';
  const unenforcedChip =
    'shrink-0 rounded-md bg-foundry-light-border/50 dark:bg-black/30 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50';
  const chip =
    'rounded-md border border-foundry-light-border dark:border-foundry-border px-2 py-0.5 font-mono text-[11px] text-foundry-light-text dark:text-foundry-text';
</script>

<div class="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto">
  <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">
    Read-only. Granting capability from the GUI is out of scope by design (DOC-57 §7.2, PM-4); edit
    <code class="font-mono">agent.toml</code>. Values are as <strong>declared</strong> in this
    agent's own file — anything inherited through <code class="font-mono">extends</code> is unioned
    at load and is not visible here yet (§7.4, PM-7).
  </p>

  <section class="flex flex-col gap-2">
    <div class="flex items-center justify-between gap-2">
      <h3 class={heading}>Tool allow-list · <code class="font-mono">[tools].allow</code></h3>
      <span class={enforcedChip}>enforced</span>
    </div>
    <p class="text-[11px] text-foundry-light-muted dark:text-foundry-text/50">
      Glob patterns gating dispatch via
      <code class="font-mono">filter_persona_tool_names</code>. Trailing
      <code class="font-mono">*</code> only.
    </p>
    {#if toolsAllow.length > 0}
      <ul class="flex flex-wrap gap-1.5">
        {#each toolsAllow as pattern (pattern)}
          <li class={chip}>{pattern}</li>
        {/each}
      </ul>
    {:else}
      <p class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px] text-foundry-amber">
        No allow-list declared. On the persona chat path this grants <strong>zero</strong> tools —
        absent denies, it does not mean unrestricted (DOC-57 §7.1).
      </p>
    {/if}
  </section>

  <section class="flex flex-col gap-2">
    <div class="flex items-center justify-between gap-2">
      <h3 class={heading}>Scopes · <code class="font-mono">[tools].scopes</code></h3>
      <span class={enforcedChip}>enforced · path-limited</span>
    </div>
    <p class="text-[11px] text-foundry-light-muted dark:text-foundry-text/50">
      RBAC / OpenRPC scope patterns, checked by <code class="font-mono">agent_can_use</code> on the
      persona-chat path. Tools discovered through the OpenRPC registry are gated; in-process tools
      declare no scope and pass unconditionally, and the
      <code class="font-mono">--direct</code> path applies no scope filtering at all (DOC-57 §7.1).
    </p>
    {#if scopes.length > 0}
      <ul class="flex flex-wrap gap-1.5">
        {#each scopes as scope (scope)}
          <li class={chip}>{scope}</li>
        {/each}
      </ul>
    {:else}
      <p class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px] text-foundry-amber">
        No scopes declared here. The gate is fail-closed for scope-carrying tools — but this agent
        may still inherit scopes from its <code class="font-mono">extends</code> base, which this
        pane cannot yet see.
      </p>
    {/if}
  </section>

  <section class="flex flex-col gap-2">
    <div class="flex items-center justify-between gap-2">
      <h3 class={heading}>User authority</h3>
      <span class={unenforcedChip}>not enforced</span>
    </div>
    <p class="text-[11px] text-foundry-light-muted dark:text-foundry-text/50">
      DOC-41 §5.5's singleton — the one agent that may act on the user's own authority, never
      inherited across <code class="font-mono">extends</code>. The field is
      <strong>reserved and does not exist</strong> in
      <code class="font-mono">AgentConfig</code> yet (#3074/#3075), so no value is shown: this pane
      would have to invent one.
    </p>
  </section>

  <section class="flex flex-col gap-2">
    <div class="flex items-center justify-between gap-2">
      <h3 class={heading}>Not surfaced yet</h3>
      <span class={unenforcedChip}>phase 4</span>
    </div>
    <p class="text-[11px] text-foundry-light-muted dark:text-foundry-text/50">
      Service tiers (<code class="font-mono">default_tier</code>,
      <code class="font-mono">unauthenticated_tier</code>), autonomy posture
      (<code class="font-mono">ask-first</code> / <code class="font-mono">learn-to-act</code>) and
      per-skill grants are specified in DOC-57 §7.2 but have no read path today —
      <code class="font-mono">[rbac]</code> parses with no read site at all. They arrive with
      <code class="font-mono">GET /api/agents/:name/permissions</code> rather than being rendered
      here as defaults nobody enforces.
    </p>
  </section>
</div>
