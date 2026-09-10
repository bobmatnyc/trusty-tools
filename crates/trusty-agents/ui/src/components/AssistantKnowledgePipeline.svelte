<script lang="ts">
  /** One protected store belongs to the selected user-facing assistant. API reports all readiness. */
  import { fetchAssistantKnowledge, reconcileAssistantKnowledge, setKnowledgePaused, extendKnowledgeHistory, type AssistantKnowledge } from '../lib/assistantKnowledge';
  let { agentName }: { agentName: string } = $props();
  let data = $state<AssistantKnowledge | null>(null);
  let busy = $state(false);
  let error = $state('');
  let requestVersion = 0;
  let pipeline = $derived(data?.pipeline);
  const status = (value: string) => value.replaceAll('_', ' ');
  const kinds = { project: 'Project', gmail: 'Gmail', gdrive: 'Google Drive', slack: 'Slack', gcal: 'Google Calendar' };
  $effect(() => {
    const name = agentName;
    data = null;
    void load(name);
    return () => { requestVersion++; };
  });
  async function load(name: string) {
    const version = ++requestVersion;
    busy = true; error = '';
    try {
      const result = await fetchAssistantKnowledge(name);
      if (version === requestVersion && name === agentName) data = result;
    } catch (cause) {
      if (version === requestVersion && name === agentName) error = String(cause instanceof Error ? cause.message : cause);
    } finally { if (version === requestVersion && name === agentName) busy = false; }
  }
  async function change(action: 'reconcile' | 'pause' | 'backfill') {
    if (busy || !data || data.assistant !== agentName) return;
    const name = agentName, current = data.pipeline, version = ++requestVersion;
    busy = true; error = '';
    try {
      const result = action === 'reconcile'
        ? await reconcileAssistantKnowledge(name, current?.revision ?? null)
        : current && (action === 'pause'
          ? await setKnowledgePaused(name, current.revision, !current.paused)
          : await extendKnowledgeHistory(name, current.revision, 1));
      if (result && version === requestVersion && name === agentName) data = result;
    } catch (cause) {
      if (version === requestVersion && name === agentName) error = String(cause instanceof Error ? cause.message : cause);
    } finally { if (version === requestVersion && name === agentName) busy = false; }
  }
</script>

<section class="knowledge-pipeline" aria-label="Assistant knowledge pipeline">
  <div class="title"><h3>Assistant knowledge</h3><button disabled={busy} onclick={() => load(agentName)}>Refresh status</button></div>
  <p>One private OKG for this assistant. Project files are indexed separately; business entities from attached projects and enabled channels belong here.</p>
  {#if error}<p role="alert" class="warning">{error}</p>{/if}
  {#if busy && !data}<p role="status">Loading assistant knowledge…</p>{/if}
  {#if data}
    {#if data.store_issue}<p class="warning">{data.store_issue}</p>{/if}
    {#if pipeline}
      <div class="store">
        <strong>{pipeline.store.protected ? 'Protected assistant OKG' : 'Store protection unavailable'}</strong>
        <code>{pipeline.store.root}</code>
        <span>Search index: <code>{pipeline.store.index_id}</code> · {data.index.connected ? 'connected' : 'not connected'}</span>
        {#if data.index.reason}<span class="warning">{data.index.reason}</span>{/if}
      </div>
      <p><strong>Requested history: {pipeline.history_months} month{pipeline.history_months === 1 ? '' : 's'}</strong> · {pipeline.paused ? 'Paused' : 'Ongoing intake enabled'}</p>
      <p>History runs in one-month windows. Requested windows do not indicate completed extraction. Incoming channel updates use the same pipeline; unavailable stages remain blocked.</p>
      <div class="actions">
        <button disabled={busy} onclick={() => change('pause')}>{pipeline.paused ? 'Resume' : 'Pause'}</button>
        <button disabled={busy || pipeline.history_months >= 120} onclick={() => change('backfill')}>Go back one month</button>
        <button disabled={busy} onclick={() => change('reconcile')}>Reconcile sources</button>
      </div>
    {:else}
      <p>This assistant’s knowledge pipeline has not been initialized.</p>
      <button disabled={busy || !!data.store_issue} onclick={() => change('reconcile')}>Initialize knowledge</button>
    {/if}
    <h4>Sources</h4>
    {#if !data.sources.length}<p>No attached project or enabled receiving channel sources.</p>{/if}
    {#each data.sources as source (source.id)}
      <div class="source">
        <strong>{source.display_name}</strong><span>{kinds[source.kind]}</span>
        {#each source.dependency_reasons as reason}<p class="warning">{reason}</p>{/each}
        {#each pipeline?.jobs.filter(job => job.source_id === source.id) ?? [] as job (job.id)}
          <details>
            <summary>{job.window.start.slice(0, 10)} → {job.window.end.slice(0, 10)} · {status(job.status)}</summary>
            <dl>
              {#each [['Raw indexing', job.indexing], ['Entity extraction', job.extraction], ['Batch cleanup', job.cleanup], ['OKG publication', job.publication]] as [label, stage]}
                <dt>{label}</dt><dd>{typeof stage === 'object' ? `${status(stage.status)} — ${stage.reason}` : ''}</dd>
              {/each}
            </dl>
            {#each job.dependency_reasons as reason}<p class="warning">{reason}</p>{/each}
          </details>
        {/each}
      </div>
    {/each}
  {/if}
</section>

<style>
  .knowledge-pipeline { display: flex; flex-direction: column; gap: .65rem; padding-bottom: 1rem; border-bottom: 1px solid var(--color-border, #aa927440); font-size: .75rem; overflow-wrap: anywhere; }
  .title, .actions { display: flex; align-items: center; justify-content: space-between; gap: .5rem; flex-wrap: wrap; }
  .actions { justify-content: flex-start; }
  h3, h4 { font-weight: 600; } h3 { font-size: .85rem; } h4 { margin-top: .3rem; }
  p, dd { opacity: .8; } .warning { color: #aa671d; }
  .store, .source { display: flex; flex-direction: column; gap: .3rem; border: 1px solid #aa927440; border-radius: .4rem; padding: .65rem; }
  code { font-size: .7rem; } button { border: 1px solid #aa927460; border-radius: .35rem; padding: .3rem .6rem; } button:disabled { opacity: .5; }
  summary { cursor: pointer; padding: .35rem 0; } dl { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: .3rem .75rem; padding: .4rem 0; }
</style>
