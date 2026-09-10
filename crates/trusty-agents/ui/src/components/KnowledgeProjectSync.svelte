<script lang="ts">
  /** Sync only explicit local chat attachments after API readiness; never replace unknown chats. */
  import { activeAgentId, agentRoster } from '../stores/app';
  import { assistantKnowledgeAttachments, setAssistantDefaultProjects } from '../stores/workspace';
  import { createKnowledgeProjectSync } from '../lib/knowledgeProjectSync';
  import { syncKnowledgeProjects, fetchAssistantKnowledge, assistantDefaultRoots } from '../lib/assistantKnowledge';
  let { ready }: { ready: boolean } = $props();
  let defaultsErrors = $state<Record<string, string | null>>({});
  let refreshDefaults: (() => void) | undefined;
  let errors = $state<Record<string, string | null>>({});
  let alive = true;
  const sync = createKnowledgeProjectSync(syncKnowledgeProjects, (assistant, error) => {
    if (alive) errors = { ...errors, [assistant]: error };
  });
  let selectedError = $derived($activeAgentId ? [defaultsErrors[$activeAgentId], errors[$activeAgentId]].filter(Boolean).join(' ') : null);
  function retry() { refreshDefaults?.(); update(); }
  function update() {
    const assistant = $activeAgentId;
    if (ready && assistant && $agentRoster.some(agent => agent.id === assistant && agent.kind === 'assistant')) {
      void sync.update(assistant, $assistantKnowledgeAttachments);
    }
  }
  $effect(() => {
    const assistant = $activeAgentId;
    if (!ready || !assistant || !$agentRoster.some(agent => agent.id === assistant && agent.kind === 'assistant')) return;
    let current = true, latest = 0;
    const refresh = () => {
      const request = ++latest;
      void fetchAssistantKnowledge(assistant).then(value => {
        if (current && request === latest) { setAssistantDefaultProjects(assistant, assistantDefaultRoots(value)); defaultsErrors = { ...defaultsErrors, [assistant]: null }; }
      }).catch(error => { if (current && request === latest) defaultsErrors = { ...defaultsErrors, [assistant]: `Saved projects could not be loaded: ${String(error)}` }; });
    };
    refreshDefaults = refresh;
    refresh();
    window.addEventListener('assistant-project-settings-changed', refresh);
    return () => { current = false; if (refreshDefaults === refresh) refreshDefaults = undefined; window.removeEventListener('assistant-project-settings-changed', refresh); };
  });
  $effect(() => { update(); });
  $effect(() => () => { alive = false; });
</script>
{#if selectedError}
  <div role="alert" class="shrink-0 border-b border-amber-600/30 px-4 py-2 text-xs text-amber-700 dark:text-amber-400">
    {selectedError}
    <button class="ml-2 underline" disabled={!ready} onclick={retry}>Retry synchronization</button>
  </div>
{/if}
