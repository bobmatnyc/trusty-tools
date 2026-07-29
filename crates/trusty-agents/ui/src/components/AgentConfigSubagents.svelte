<script lang="ts">
  /**
   * Why (#4029, epic #4021): the SIXTH configuration section — "who else can
   * this agent get to do the work". Owner decision, 2026-07-26, resolving
   * OQ-5: *"Sub-agents is its own configuration section"*; refined 2026-07-27
   * to require the UNION of BOTH delegation mechanisms, which no spec had
   * previously reconciled.
   *
   * The two are not variants of one thing and this pane never merges them:
   * `delegate_to_agent` is in-product (trusty-agents → trusty-agents), its
   * target set a HARDCODED role allowlist crossed with the resolvable roster
   * and narrowed by #4169's L0/L1 tier gate — no per-agent config at all;
   * `dispatch_task` is cross-product (trusty-agents → trusty-code), the one
   * with a real `[subagents].allowed` binding, intersected fail-closed with
   * the bridge's own `NON_CODING_TARGETS` floor (OQ-7). Their target
   * vocabularies do not overlap — `documentation` is an in-product role and
   * appears nowhere else — so a single flat list could not tell an operator
   * which enforcement layer applies to a given name.
   *
   * What: two labelled groups over `GET /api/agents/:name/subagents`, which
   * resolves both through the SAME code the enforcement points call. Read-only,
   * matching every other read-only section (DOC-57 §7.2's PM-4 rationale
   * applies at least as strongly here: delegation reach is the escalation
   * surface #3555 and #4169 exist to close).
   *
   * Honesty rules, mirroring the sibling panes: `tool_registered` and
   * `tool_granted` render separately (registered ≠ granted); an unreachable
   * target is SHOWN with the gate's reason rather than hidden, so "why can't my
   * assistant reach that one" is answerable; and an unresolvable config renders
   * the denial plus the parse error, never a blank pane implying "nothing
   * configured".
   *
   * DOC-57 still documents five sections; #4182 amends it. This pane cites the
   * decision, it does not restate the spec.
   * Test: `AgentConfigSubagents.test.ts`.
   */
  import type { AgentSubagents } from '../lib/agentConfig';

  /** Loaded by the shell from `GET /api/agents/:name/subagents`; `null` while loading. */
  export let data: AgentSubagents | null = null;
  /** Non-empty when the fetch itself failed. */
  export let error = '';

  // Every field is defaulted rather than assumed: an older sidecar (or a route
  // that degraded) can return a payload missing a key, and a pane that throws on
  // that shows the user nothing at all — strictly worse than showing what did
  // arrive. Same posture as `AgentConfigSkills`.
  $: inProduct = data?.in_product ?? null;
  $: crossProduct = data?.cross_product ?? null;
  $: inTargets = inProduct?.targets ?? [];
  $: reachable = inTargets.filter((t) => t.reachable);
  $: blocked = inTargets.filter((t) => !t.reachable);
  $: unresolved = inProduct?.unresolved ?? [];
  $: crossTargets = crossProduct?.targets ?? [];
  $: crossGranted = crossTargets.filter((t) => t.granted);
  $: rejected = crossProduct?.rejected ?? [];

  const heading =
    'shrink-0 font-mono text-[10px] font-semibold uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50';
  const okChip =
    'shrink-0 rounded-md bg-foundry-teal/15 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-foundry-teal';
  const offChip =
    'shrink-0 rounded-md bg-foundry-light-border/50 dark:bg-black/30 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50';
  const tag =
    'rounded border border-foundry-light-border dark:border-foundry-border px-1.5 py-0.5 font-mono text-[10px] text-foundry-light-text dark:text-foundry-text';
</script>

<div class="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto">
  <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">
    Who this agent can hand work to. Two independent mechanisms with two different
    enforcement layers — they are shown separately because a name reachable through one is not
    reachable through the other. Read-only; edit
    <code class="font-mono">[subagents]</code> in <code class="font-mono">agent.toml</code>. The
    in-product target set is not configurable at all — it is a fixed role allow-list plus the
    L0/L1 delegation gate.
  </p>

  {#if error}
    <p class="rounded-md border border-foundry-red/40 px-3 py-2 text-[11px] text-foundry-red">
      Could not load sub-agents: {error}
    </p>
  {:else if !data}
    <p class="text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
      Resolving sub-agents…
    </p>
  {:else}
    {#if data.config_error}
      <p class="rounded-md border border-foundry-amber/40 px-3 py-2 text-[11px] text-foundry-amber">
        <code class="font-mono">agent.toml</code> could not be resolved, so no delegation grant
        could be read — everything below is denied, fail-closed: {data.config_error}
      </p>
    {/if}

    <!-- ── In-product: delegate_to_agent ────────────────────────────────── -->
    <section class="flex flex-col gap-2">
      <div class="flex items-center justify-between gap-2">
        <h3 class={heading}>
          In-product · <code class="font-mono">{inProduct?.tool ?? 'delegate_to_agent'}</code>
        </h3>
        <span class={inProduct?.tool_granted ? okChip : offChip}>
          {inProduct?.tool_granted ? 'granted' : 'not granted'}
        </span>
      </div>
      <p class="text-[11px] text-foundry-light-muted dark:text-foundry-text/50">
        Another trusty-agents agent, run in-process. Targets are fixed by the role allow-list
        <span class="font-mono">({(inProduct?.allowed_roles ?? []).join(', ')})</span> — not by this
        agent's configuration — and further narrowed by the one-directional L0/L1 delegation gate.
        This agent's own tier is <code class="font-mono">{inProduct?.delegator_tier ?? 'l1'}</code>.
      </p>

      {#if !inProduct?.tool_registered}
        <p class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
          <code class="font-mono">delegate_to_agent</code> is not registered for this agent's role
          — only assistant-tier personas get it. Nothing below is callable.
        </p>
      {:else if !inProduct?.tool_granted}
        <p class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px] text-foundry-amber">
          The tool is registered for this role but this agent's
          <code class="font-mono">[tools].allow</code> does not grant it, so dispatch denies every
          target below.
        </p>
      {/if}

      {#if reachable.length === 0 && blocked.length === 0}
        <p class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
          No agent with a delegatable role resolves on this host.
        </p>
      {/if}

      {#each reachable as t (t.name)}
        <div class="rounded-md border border-foundry-light-border dark:border-foundry-border px-3 py-2">
          <div class="flex items-baseline justify-between gap-2">
            <span class="text-xs font-semibold text-foundry-light-text dark:text-foundry-text">
              {t.display_name || t.name}
            </span>
            <span class={okChip}>reachable</span>
          </div>
          <div class="mt-1.5 flex flex-wrap items-center gap-1">
            <span class={tag} title="The agent id delegate_to_agent names">{t.name}</span>
            <span class={tag} title="The role the allow-list gates on">{t.role}</span>
            <span class={tag} title="Privilege tier (#4168)">tier {t.tier}</span>
          </div>
        </div>
      {/each}

      {#if blocked.length > 0}
        <div class="rounded-md border border-foundry-amber/40 px-3 py-2">
          <h4 class="font-mono text-[10px] uppercase tracking-wide text-foundry-amber">
            Refused by the delegation gate ({blocked.length})
          </h4>
          {#each blocked as t (t.name)}
            <p class="mt-1 text-[11px] text-foundry-light-muted dark:text-foundry-text/60">
              <code class="font-mono">{t.name}</code>
              <span class="font-mono">(tier {t.tier})</span> — {t.reason}
            </p>
          {/each}
        </div>
      {/if}

      {#if unresolved.length > 0}
        <details class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2">
          <summary class="cursor-pointer font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/40">
            {unresolved.length} agent{unresolved.length === 1 ? '' : 's'} whose config did not resolve
          </summary>
          {#each unresolved as u (u.name)}
            <p class="mt-1 text-[11px] text-foundry-light-muted dark:text-foundry-text/60">
              <code class="font-mono">{u.name}</code> — {u.reason}
            </p>
          {/each}
        </details>
      {/if}

      {#if (inProduct?.role_excluded_count ?? 0) > 0}
        <p class="text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
          {inProduct?.role_excluded_count} further agent(s) on this host are excluded by the role
          allow-list and are not delegation targets.
        </p>
      {/if}

      <!-- #4235: hidden agents are omitted from the target cards above, so the
           pane says how many rather than leaving them silently missing. -->
      {#if (inProduct?.hidden_excluded_count ?? 0) > 0}
        <p class="text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
          {inProduct?.hidden_excluded_count} further agent(s) on this host are hidden
          (<code class="font-mono">hidden = true</code>) and are not listed here.
        </p>
      {/if}
    </section>

    <!-- ── Cross-product: dispatch_task ─────────────────────────────────── -->
    <section class="flex flex-col gap-2">
      <div class="flex items-center justify-between gap-2">
        <h3 class={heading}>
          Cross-product · <code class="font-mono">{crossProduct?.tool ?? 'dispatch_task'}</code>
        </h3>
        <span class={crossProduct?.tool_granted ? okChip : offChip}>
          {crossProduct?.tool_granted ? 'granted' : 'not granted'}
        </span>
      </div>
      <p class="text-[11px] text-foundry-light-muted dark:text-foundry-text/50">
        A NON-CODING specialist from trusty-code, run out-of-process; its result is always a
        proposal, never an authorization. This is the half
        <code class="font-mono">[subagents].allowed</code> configures — intersected with the
        bridge's own floor
        <span class="font-mono">({(crossProduct?.bridge_floor ?? []).join(', ')})</span>, which
        configuration can narrow but never widen.
      </p>

      {#if !crossProduct?.declares_allowed}
        <p class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
          This agent declares no <code class="font-mono">[subagents]</code> section. Absent grants
          <strong>nothing</strong> — it does not mean "all" (OQ-7, fail-closed).
        </p>
      {/if}

      {#each crossTargets as t (t.name)}
        <div class="rounded-md border border-foundry-light-border dark:border-foundry-border px-3 py-2">
          <div class="flex items-baseline justify-between gap-2">
            <span class="text-xs font-semibold text-foundry-light-text dark:text-foundry-text">
              {t.name}
            </span>
            <span class={t.granted ? okChip : offChip}>
              {t.granted ? 'granted' : 'denied'}
            </span>
          </div>
          {#if t.reason}
            <p class="mt-0.5 text-[11px] text-foundry-light-muted dark:text-foundry-text/60">
              {t.reason}
            </p>
          {/if}
        </div>
      {/each}

      {#if rejected.length > 0}
        <div class="rounded-md border border-foundry-amber/40 px-3 py-2">
          <h4 class="font-mono text-[10px] uppercase tracking-wide text-foundry-amber">
            Declared but refused by the bridge floor ({rejected.length})
          </h4>
          {#each rejected as r (r.name)}
            <p class="mt-1 text-[11px] text-foundry-light-muted dark:text-foundry-text/60">
              <code class="font-mono">{r.name}</code> — {r.reason}
            </p>
          {/each}
        </div>
      {/if}

      <p class="text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
        {crossGranted.length} of {crossTargets.length} cross-product specialists granted.
      </p>
    </section>
  {/if}
</div>
