<script lang="ts">
  /**
   * Why (#7454, ADR-0060, DOC-57 §4.4): an MCP connection is configured
   * globally OR per assistant, and before this pane the assistant tier existed
   * only as a hand-edited `[mcp]` table in a file most users will never open.
   * The Knowledge pane already claims to answer "what does this agent know" —
   * showing only the global list there would have been wrong the moment an
   * assistant overrode anything.
   *
   * What: the global connections with a per-assistant switch, this assistant's
   * own added servers, and the EFFECTIVE set with a per-server status. A server
   * that is configured but not usable — disabled, or a credential that does not
   * resolve — renders with its reason rather than disappearing, which is the
   * same honesty rule §4.4 applies to a disabled endpoint (C-03.2).
   *
   * Degrades the way the rest of the config panel does: distinct loading /
   * failed / loaded states, and a failed save leaves the previous selection on
   * screen rather than a half-applied one.
   * Test: `AssistantMcpConnections.test.ts`.
   */
  import {
    fetchAssistantMcp,
    saveAssistantMcpDisabled,
    transportLabel,
    type AssistantMcp,
  } from '../lib/assistantMcp';

  let { agentName }: { agentName: string } = $props();
  let data = $state<AssistantMcp | null>(null);
  let busy = $state(false);
  let error = $state('');
  let requestVersion = 0;

  $effect(() => {
    const name = agentName;
    data = null;
    void load(name);
    return () => {
      requestVersion++;
    };
  });

  async function load(name: string) {
    const version = ++requestVersion;
    busy = true;
    error = '';
    try {
      const result = await fetchAssistantMcp(name);
      if (version === requestVersion && name === agentName) data = result;
    } catch (cause) {
      if (version === requestVersion && name === agentName)
        error = String(cause instanceof Error ? cause.message : cause);
    } finally {
      if (version === requestVersion && name === agentName) busy = false;
    }
  }

  async function toggle(server: string, connected: boolean) {
    if (busy || !data || data.assistant !== agentName) return;
    const name = agentName;
    const next = connected
      ? data.overrides.disabled.filter((id) => id !== server)
      : [...data.overrides.disabled, server];
    const version = ++requestVersion;
    busy = true;
    error = '';
    try {
      const result = await saveAssistantMcpDisabled(name, data, next);
      if (version === requestVersion && name === agentName) data = result;
    } catch (cause) {
      if (version === requestVersion && name === agentName)
        error = String(cause instanceof Error ? cause.message : cause);
    } finally {
      if (version === requestVersion && name === agentName) busy = false;
    }
  }

  const heading =
    'shrink-0 font-mono text-[10px] font-semibold uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50';
</script>

<section class="flex flex-col gap-2" aria-label="Assistant MCP connections">
  <h3 class={heading}>MCP connections</h3>
  {#if error}
    <p role="alert" class="text-[11px] text-foundry-amber">{error}</p>
  {/if}
  {#if !data}
    {#if busy}<p role="status" class="text-xs text-foundry-light-muted dark:text-foundry-text/60">Resolving MCP connections…</p>{/if}
  {:else}
    {#each data.issues as issue (issue.path)}
      <p role="alert" class="rounded-md border border-foundry-amber/40 bg-foundry-amber/10 px-3 py-2 text-[11px] text-foundry-amber">
        {issue.detail} <span class="font-mono">({issue.path})</span> — {issue.remedy}
      </p>
    {/each}

    <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">
      Shared connections come from
      <code class="font-mono">~/.trusty-tools/mcp/servers.toml</code>. Switching one off here
      applies to this assistant only.
    </p>
    {#if data.global.length === 0}
      <p class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
        No shared MCP servers are configured yet.
      </p>
    {/if}
    {#each data.global as server (server.name)}
      <label class="flex items-center gap-2 text-xs text-foundry-light-text dark:text-foundry-text">
        <input
          type="checkbox"
          disabled={busy}
          checked={!data.overrides.disabled.includes(server.name)}
          onchange={(event) => toggle(server.name, (event.currentTarget as HTMLInputElement).checked)}
        />
        <span class="font-mono">{server.name}</span>
        <span class="font-mono text-[10px] text-foundry-light-muted dark:text-foundry-text/40">
          {transportLabel(server)}
        </span>
      </label>
    {/each}

    {#if data.overrides.servers.length > 0}
      <h3 class={heading}>Added for this assistant</h3>
      {#each data.overrides.servers as server (server.name)}
        <p class="font-mono text-[11px] text-foundry-light-text dark:text-foundry-text">
          {server.name} — {transportLabel(server)}
        </p>
      {/each}
    {/if}

    <h3 class={heading}>Effective connections</h3>
    {#if data.statuses.length === 0}
      <p class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
        This assistant connects to no MCP server.
      </p>
    {/if}
    {#each data.statuses as status (status.name)}
      <div class="flex flex-col gap-0.5 rounded-md border border-foundry-light-border dark:border-foundry-border px-3 py-2">
        <div class="flex items-center justify-between gap-2">
          <span class="font-mono text-xs font-semibold text-foundry-light-text dark:text-foundry-text">{status.name}</span>
          <span
            class="shrink-0 rounded-md px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide {status.usable
              ? 'bg-foundry-teal/15 text-foundry-teal'
              : 'bg-foundry-light-border/50 dark:bg-black/30 text-foundry-light-muted dark:text-foundry-text/50'}"
          >
            {status.usable ? 'available' : 'unavailable'}
          </span>
        </div>
        <span class="font-mono text-[11px] text-foundry-light-muted dark:text-foundry-text/50">
          {status.tier === 'assistant' ? 'set for this assistant' : 'shared'}
        </span>
        {#if status.reason}
          <span class="text-[11px] text-foundry-amber">{status.reason}</span>
        {/if}
      </div>
    {/each}
  {/if}
</section>
