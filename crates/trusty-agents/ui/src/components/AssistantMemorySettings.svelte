<script lang="ts">
  import { fetchAssistantMemoryPolicy as fetchAssistantMemory, patchAssistantMemory, SettingsConflict, type AssistantMemoryPolicy } from '../lib/assistantMemoryPolicy';
  let { agentName }: { agentName: string } = $props();
  let policy = $state<AssistantMemoryPolicy | null>(null);
  let error = $state('');
  let saving = $state(false);
  let generation = 0;
  $effect(() => {
    const owner = agentName, request = ++generation;
    policy = null; error = ''; saving = false;
    void fetchAssistantMemory(owner).then(value => { if (request === generation) policy = value; })
      .catch(cause => { if (request === generation) error = String(cause); });
    return () => { generation++; };
  });
  async function change(event: Event) {
    if (!policy || saving) return;
    const input = event.target as HTMLInputElement;
    const enabled = input.checked, owner = agentName, request = generation;
    input.checked = policy.cross_palace_query;
    saving = true; error = '';
    try {
      const value = await patchAssistantMemory(owner, policy.revision, enabled);
      if (request === generation) policy = value;
    } catch (cause) {
      if (request !== generation) return;
      error = String(cause);
      if (cause instanceof SettingsConflict) {
        try {
          const value = await fetchAssistantMemory(owner);
          if (request === generation) { policy = value; error = 'Memory settings changed elsewhere. Current settings loaded; repeat your change to save it.'; }
        } catch (reloadError) { if (request === generation) { policy = null; error = String(reloadError); } }
      }
    } finally { if (request === generation) saving = false; }
  }
</script>
<section class="space-y-2 rounded border border-foundry-light-border dark:border-foundry-border p-3" aria-label="Assistant memory settings">
  <h3 class="text-sm font-semibold">Memory</h3>
  {#if policy}
    <p class="text-xs">Memory namespace: <code>{policy.namespace}</code></p>
    <label class="flex items-center gap-2 text-xs">
      <input type="checkbox" checked={policy.cross_palace_query} disabled={saving} onchange={change} />
      Allow queries across memory palaces
    </label>
    <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">Memory is stored by trusty-memory. Writes always stay in this assistant’s namespace.</p>
    {#if saving}<p class="text-xs" role="status">Saving memory settings…</p>{/if}
  {:else if !error}<p class="text-xs" role="status">Loading memory settings…</p>{/if}
  {#if error}<p class="text-xs text-red-600 dark:text-red-400" role="alert">{error}</p>{/if}
</section>
