<script lang="ts">
  /**
   * Why (#7428, product spec §1a item 3): each Assistant has ONE memory palace,
   * and reading another Assistant's palace is a setting the user turns on — it
   * is never automatic. Without this pane the fan-out would exist only as a
   * hand-edited `[memory] fan_out` line in the assistant's `config.toml`, which
   * is not a setting a user can be expected to find.
   *
   * What: the RESOLVED own palace (a palace is derived, so the stored value is
   * routinely empty for an assistant that has one — rendering the stored value
   * would read as "no memory"), plus a checkbox list over the other assistants.
   * Selecting one grants READ access to its palace; writes always stay in this
   * assistant's own palace, which the pane says outright.
   *
   * Degrades the way the rest of the config panel does: three distinct states
   * for loading / failed / loaded, and a failed save leaves the previous
   * selection on screen rather than a half-applied one.
   * Test: `AssistantMemoryFanOut.test.ts`.
   */
  import { fetchAssistantMemory, saveAssistantFanOut, type AssistantMemory } from '../lib/assistantMemory';

  let { agentName }: { agentName: string } = $props();
  let data = $state<AssistantMemory | null>(null);
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
      const result = await fetchAssistantMemory(name);
      if (version === requestVersion && name === agentName) data = result;
    } catch (cause) {
      if (version === requestVersion && name === agentName)
        error = String(cause instanceof Error ? cause.message : cause);
    } finally {
      if (version === requestVersion && name === agentName) busy = false;
    }
  }

  async function toggle(other: string, selected: boolean) {
    if (busy || !data || data.assistant !== agentName) return;
    const name = agentName;
    const next = selected ? [...data.fan_out, other] : data.fan_out.filter((id) => id !== other);
    const version = ++requestVersion;
    busy = true;
    error = '';
    try {
      const result = await saveAssistantFanOut(name, data, next);
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

<section class="flex flex-col gap-2" aria-label="Assistant memory">
  <h3 class={heading}>Memory palace</h3>
  {#if error}
    <p role="alert" class="text-[11px] text-foundry-amber">{error}</p>
  {/if}
  {#if !data}
    {#if busy}<p role="status" class="text-xs text-foundry-light-muted dark:text-foundry-text/60">Resolving memory settings…</p>{/if}
  {:else}
    <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">
      This assistant remembers in
      <code class="font-mono">{data.resolved.own ?? 'no palace'}</code>
      {#if data.resolved.source === 'binding'}
        (pinned by <code class="font-mono">[[stores]]</code> in
        <code class="font-mono">agent.toml</code>)
      {:else if data.resolved.source === 'config'}
        (set in memory settings)
      {:else if data.resolved.source === 'instance-id'}
        (named after the assistant)
      {/if}. Everything it stores goes here and nowhere else.
    </p>

    <h3 class={heading}>Also read from</h3>
    <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">
      Selecting another assistant lets this one READ that assistant's memory. It never writes there,
      and nothing is shared until you select it.
    </p>
    {#if data.available.length === 0}
      <p class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
        There is no other assistant to read from yet.
      </p>
    {/if}
    {#each data.available as other (other)}
      <label class="flex items-center gap-2 text-xs text-foundry-light-text dark:text-foundry-text">
        <input
          type="checkbox"
          disabled={busy}
          checked={data.fan_out.includes(other)}
          onchange={(event) => toggle(other, (event.currentTarget as HTMLInputElement).checked)}
        />
        <span class="font-mono">{other}</span>
      </label>
    {/each}
    {#if data.resolved.fan_out_palaces.length > 0}
      <p class="font-mono text-[11px] text-foundry-light-muted dark:text-foundry-text/50">
        reading palaces: {data.resolved.fan_out_palaces.join(', ')}
      </p>
    {/if}
  {/if}
</section>
