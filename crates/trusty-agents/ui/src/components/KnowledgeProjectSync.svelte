<script lang="ts">
  /** Sync only explicit local chat attachments after API readiness; never replace unknown chats. */
  import { activeAgentId, agentRoster } from '../stores/app';
  import { assistantKnowledgeAttachments } from '../stores/workspace';
  import { createKnowledgeProjectSync } from '../lib/knowledgeProjectSync';
  import { syncKnowledgeProjects } from '../lib/assistantKnowledge';
  let { ready }: { ready: boolean } = $props();
  let errors = $state<Record<string, string | null>>({});
  let alive = true;
  const sync = createKnowledgeProjectSync(syncKnowledgeProjects, (assistant, error) => {
    if (alive) errors = { ...errors, [assistant]: error };
  });
  let selectedError = $derived($activeAgentId ? errors[$activeAgentId] : null);
  function update() {
    const assistant = $activeAgentId;
    if (ready && assistant && $agentRoster.some(agent => agent.id === assistant && agent.kind === 'assistant')) {
      void sync.update(assistant, $assistantKnowledgeAttachments);
    }
  }
  $effect(() => { update(); });
  $effect(() => () => { alive = false; });
</script>
{#if selectedError}
  <div role="alert" class="shrink-0 border-b border-amber-600/30 px-4 py-2 text-xs text-amber-700 dark:text-amber-400">
    Project attachments are saved locally, but assistant knowledge could not be synchronized: {selectedError}
    <button class="ml-2 underline" disabled={!ready} onclick={update}>Retry synchronization</button>
  </div>
{/if}
