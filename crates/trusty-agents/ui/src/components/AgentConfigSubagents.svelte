<script lang="ts">
  /** Editable in-product delegation whitelist. Eligibility is reported by
   * the server using the same kind/tier/floor gates as dispatch. */
  import { onDestroy } from 'svelte';
  import { patchAgent, fetchAgentSubagents, type AgentSubagents } from '../lib/agentConfig';
  import AgentConfigCoding from './AgentConfigCoding.svelte';

  /** Loaded by the shell from `GET /api/agents/:name/subagents`; `null` while loading. */
  export let data: AgentSubagents | null = null;
  /** Non-empty when the fetch itself failed. */
  export let error = '';
  export let agentName = '';
  let viewData: AgentSubagents | null = null;
  let previousData: AgentSubagents | null | undefined = undefined, previousAgent = '';
  let selected: string[] = [];
  let saving = false, saveError = '', notice = '';
  let generation = 0;
  $: if (data !== previousData || agentName !== previousAgent) {
    previousData = data; previousAgent = agentName; generation++;
    viewData = data; selected = selection(data); saving = false; saveError = ''; notice = '';
  }
  function selection(value: AgentSubagents | null): string[] {
    return value?.in_product.selected ?? value?.in_product.targets.filter(target => (target.selected ?? target.reachable) && (value.in_product.reachable_floor ?? []).includes(target.name)).map(target => target.name) ?? [];
  }
  async function reload() {
    const token = ++generation, name = agentName; saving = true; saveError = ''; notice = '';
    try { const next = await fetchAgentSubagents(name); if (token === generation) { viewData = next; selected = selection(next); } }
    catch (cause) { if (token === generation) saveError = 'Could not reload delegation settings. ' + String(cause); }
    finally { if (token === generation) saving = false; }
  }
  async function toggle(name: string, enabled: boolean) {
    if (saving || !agentName) return;
    selected = enabled ? [...new Set([...selected, name])] : selected.filter(item => item !== name);
    const token = generation, owner = agentName, next = [...selected];
    saving = true; saveError = ''; notice = '';
    try {
      await patchAgent(owner, { subagents_delegate_allowed: next });
      if (token !== generation) return;
      const refreshed = await fetchAgentSubagents(owner);
      if (token !== generation) return;
      viewData = refreshed; selected = selection(refreshed); notice = 'Saved. The selected whitelist applies to new turns.';
    } catch (cause) { if (token === generation) saveError = 'Could not confirm the saved whitelist. Your selection is still shown; reload to check. ' + String(cause); }
    finally { if (token === generation) saving = false; }
  }
  onDestroy(() => generation++);

  // Every field is defaulted rather than assumed: an older sidecar (or a route
  // that degraded) can return a payload missing a key, and a pane that throws on
  // that shows the user nothing at all — strictly worse than showing what did
  // arrive. Same posture as `AgentConfigSkills`.
  $: inProduct = viewData?.in_product ?? null;
  $: crossProduct = viewData?.cross_product ?? null;
  $: inTargets = inProduct?.targets ?? [];
  $: candidates = inTargets.filter(target => target.eligible ?? (target.reachable && (inProduct?.reachable_floor ?? []).includes(target.name)));
  $: crossTargets = crossProduct?.targets ?? [];
  $: crossGranted = crossTargets.filter((t) => t.granted);
  $: rejected = crossProduct?.rejected ?? [];

  const heading =
    'shrink-0 font-mono text-[10px] font-semibold uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50';
  const okChip =
    'shrink-0 rounded-md bg-foundry-teal/15 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-foundry-teal';
  const offChip =
    'shrink-0 rounded-md bg-foundry-light-border/50 dark:bg-black/30 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50';
</script>

<div class="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto">
  <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">Choose which agents this assistant may delegate work to. Only eligible agents are offered.</p>
  {#if error}
    <p class="rounded-md border border-foundry-red/40 px-3 py-2 text-[11px] text-foundry-red">
      Could not load sub-agents: {error}
    </p>
  {:else if !viewData}
    <p class="text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
      Resolving sub-agents…
    </p>
  {:else}
    {#if viewData.config_error}
      <p class="rounded-md border border-foundry-amber/40 px-3 py-2 text-[11px] text-foundry-amber">
        <code class="font-mono">agent.toml</code> could not be resolved, so no delegation grant
        could be read — everything below is denied, fail-closed: {viewData.config_error}
      </p>
    {/if}

    <section class="flex flex-col gap-3" aria-label="Delegation whitelist">
      <h3 class={heading}>Allowed agents</h3>
      {#if !inProduct?.tool_registered}
        <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">Delegation is not available for this agent's role.</p>
      {:else}
        {#if !inProduct.tool_granted}<p class="text-xs text-foundry-amber">Delegation is not enabled in this assistant's tool permissions. The saved list will apply once delegation is enabled.</p>{/if}
        {#if candidates.length === 0}<p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">No eligible delegation agents are installed.</p>{/if}
        {#each candidates as target (target.name)}
          <label class="flex items-start gap-3 rounded-md border border-foundry-light-border dark:border-foundry-border px-3 py-2">
            <input type="checkbox" class="mt-0.5" checked={selected.includes(target.name)} disabled={saving || !agentName || !!viewData.config_error} on:change={event => toggle(target.name, event.currentTarget.checked)} />
            <span class="min-w-0"><span class="block text-xs font-semibold text-foundry-light-text dark:text-foundry-text">{target.display_name || target.name}</span><span class="block text-[11px] text-foundry-light-muted dark:text-foundry-text/60">{target.name} · {target.role}</span></span>
          </label>
        {/each}
        {#if selected.length === 0}<p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">No agents selected. This assistant will not delegate to other agents.</p>{/if}
      {/if}
      {#if saving}<p role="status" class="text-xs">Saving delegation settings…</p>{/if}
      {#if notice}<p role="status" class="text-xs">{notice}</p>{/if}
      {#if saveError}<p role="alert" class="text-xs text-foundry-amber">{saveError}</p><button type="button" class="text-left text-xs underline" disabled={saving} on:click={reload}>Reload delegation settings</button>{/if}
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

    <!-- ── Coding: dispatch_task → the tcode project manager (#4353) ─────── -->
    <AgentConfigCoding data={viewData.coding} />
  {/if}
</div>
