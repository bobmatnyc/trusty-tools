<script lang="ts">
  import { onDestroy } from 'svelte';
  import { Plus, Trash2, RefreshCw } from 'lucide-svelte';
  import { fetchListeners, saveListeners, type ListenerConfiguration } from '../lib/listeners';
  export let agentName: string;
  export let dirty = false;
  export let saving = false;
  interface Draft { name: string; enabled: boolean; event_types: string; from: string; include_labels: string; exclude_labels: string; subject_contains: string; snippet_contains: string; instructions: string }
  const fields = [
    ['event_types', 'Event types', 'message.received'], ['from', 'Senders', '*@example.com'],
    ['include_labels', 'Include labels', 'INBOX'], ['exclude_labels', 'Exclude labels', 'CATEGORY_PROMOTIONS'],
    ['subject_contains', 'Subject contains', 'Project update'], ['snippet_contains', 'Preview contains', 'Action required'],
  ] as const;
  let data: ListenerConfiguration | null = null, drafts: Draft[] = [];
  let baseline = '[]', loadedAgent = '', loading = false, error = '', notice = '', selected = '';
  let generation = 0;
  $: dirty = JSON.stringify(drafts) !== baseline;
  $: available = (data?.available_listeners ?? []).filter(item => !drafts.some(draft => draft.name === item.name));
  $: if (agentName !== loadedAgent) { loadedAgent = agentName; void load(); }
  function apply(next: ListenerConfiguration) {
    data = next;
    drafts = next.listeners.map(item => ({ name: item.name, enabled: item.enabled ?? true, event_types: item.event_types.join('\n'),
      from: item.filter.from.join('\n'), include_labels: (item.filter.include_labels ?? []).join('\n'), exclude_labels: item.filter.exclude_labels.join('\n'),
      subject_contains: (item.filter.subject_contains ?? []).join('\n'), snippet_contains: (item.filter.snippet_contains ?? []).join('\n'), instructions: item.instructions ?? '' }));
    baseline = JSON.stringify(drafts);
  }
  async function load() {
    const token = ++generation, agent = agentName;
    loading = true; saving = false; error = ''; notice = ''; data = null; drafts = []; baseline = '[]';
    try { const next = await fetchListeners(agent); if (token === generation) apply(next); }
    catch (cause) { if (token === generation) error = cause instanceof Error ? cause.message : String(cause); }
    finally { if (token === generation) loading = false; }
  }
  const values = (text: string) => [...new Set(text.split('\n').map(value => value.trim()).filter(Boolean))];
  export async function save(): Promise<boolean> {
    if (!data || saving) return false;
    if (!dirty) return true;
    const token = generation, agent = agentName;
    saving = true; error = ''; notice = '';
    const bindings = drafts.map(item => ({ name: item.name, enabled: item.enabled, event_types: values(item.event_types),
      filter: { from: values(item.from), include_labels: values(item.include_labels), exclude_labels: values(item.exclude_labels), subject_contains: values(item.subject_contains), snippet_contains: values(item.snippet_contains) }, instructions: item.instructions }));
    try {
      const next = await saveListeners(agent, data.revision, bindings);
      if (token !== generation) return false;
      apply(next); notice = 'Listener settings saved.'; return true;
    } catch (cause) { if (token === generation) error = cause instanceof Error ? cause.message : String(cause); return false; }
    finally { if (token === generation) saving = false; }
  }
  function add() {
    if (!selected || !available.some(item => item.name === selected)) return;
    drafts = [...drafts, { name: selected, enabled: false, event_types: '', from: '', include_labels: '', exclude_labels: '', subject_contains: '', snippet_contains: '', instructions: '' }]; selected = '';
  }
  onDestroy(() => { generation++; });
</script>
<div class="listeners">
  <p class="help">Filters decide which events reach this assistant before it runs. Values within a field match any; filled fields must all match. Excluded labels always block an event.</p>
  <p class="help">One value per line. Empty filters allow any value. Sender patterns support a leading or trailing *. Subject and preview matching use literal text, ignoring case.</p>
  {#if loading}<p role="status">Loading listeners…</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if data}
    <fieldset disabled={saving}>
      <div class="add-row"><label for="available-listener">Add listener</label><select id="available-listener" bind:value={selected}><option value="">Select a configured listener…</option>{#each available as source}<option value={source.name}>{source.name} ({source.connector}){source.enabled ? '' : ' — paused'}</option>{/each}</select><button type="button" aria-label="Add listener binding" disabled={!selected || drafts.length >= 32} on:click={add}><Plus size={16} /></button></div>
      {#if !drafts.length}<p class="help">This assistant has no listener bindings. Add a configured listener to choose the events it receives.</p>{/if}
      {#if !data.available_listeners.length}<p class="help">No event sources are configured. Configure a source before adding a binding.</p>{/if}
      {#each drafts as draft, index (draft.name)}
        {@const source = data.available_listeners.find(item => item.name === draft.name)}
        {@const inherited = data.inherited_names?.includes(draft.name) ?? false}
        <section class="binding" aria-label={`Listener ${draft.name}`}>
          <header><h3>{draft.name}</h3><label class="enabled"><input type="checkbox" bind:checked={draft.enabled} />Receive events</label>{#if !inherited}<button type="button" aria-label={`Remove ${draft.name}`} on:click={() => drafts = drafts.filter((_, i) => i !== index)}><Trash2 size={15} /></button>{/if}</header>
          {#if inherited}<p class="help">Provided by the assistant template. Turn off Receive events to disable it for this assistant.</p>{/if}
          {#if !source}<p class="error">This source is no longer configured. Disable or remove this binding before saving.</p>{:else}<p class="help">{source.connector}{source.identity ? ` · ${source.identity}` : ''}{source.enabled ? '' : ' · Source paused; events will not arrive until it is enabled.'}</p>{/if}
          <div class="filters">{#each fields as [key, label, example]}<label>{label}<textarea rows="2" bind:value={draft[key]} placeholder={example} aria-label={`${draft.name} ${label}`}></textarea></label>{/each}</div>
          <label class="instructions">Instructions for this listener<textarea rows="5" maxlength="8000" bind:value={draft.instructions} aria-label={`${draft.name} instructions`} placeholder="Describe how the assistant should respond to matching events…"></textarea></label>
          <p class="help">These instructions apply after filtering. You can also ask this assistant to configure its listeners.</p>
        </section>
      {/each}
    </fieldset>
  {/if}
  <footer><button type="button" class="save" disabled={!data || !dirty || saving || loading} on:click={save}>{saving ? 'Saving…' : 'Save listeners'}</button><button type="button" disabled={saving || loading} on:click={load}><RefreshCw size={13} />{dirty ? 'Discard changes and reload' : 'Reload'}</button>{#if notice}<span role="status">{notice}</span>{/if}{#if dirty}<span class="help">Unsaved changes</span>{/if}</footer>
</div>
<style>
  .listeners { min-height:0; flex:1; overflow:auto; color:rgb(var(--color-text-primary)); font-size:12px; }
  .help { opacity:.65; font-size:11px; margin:5px 0 10px; } .error { color:rgb(var(--color-warning)); margin:8px 0; }
  fieldset { min-width:0; } .add-row,header,footer { display:flex; align-items:center; gap:10px; flex-wrap:wrap; }
  .add-row { margin:16px 0; } .binding { border:1px solid rgb(var(--color-border)); border-radius:7px; padding:14px; margin-bottom:14px; }
  h3 { font-weight:600; flex:1; overflow-wrap:anywhere; } .enabled { display:flex; align-items:center; gap:5px; }
  .filters { display:grid; grid-template-columns:repeat(auto-fit,minmax(min(220px,100%),1fr)); gap:12px; }
  .filters label,.instructions { display:flex; flex-direction:column; gap:5px; } .instructions { margin-top:12px; }
  textarea,select { min-width:0; padding:7px; border:1px solid rgb(var(--color-border)); border-radius:4px; color:inherit; background:rgb(var(--color-card-bg)); }
  textarea { resize:vertical; width:100%; } select { max-width:100%; } button { cursor:pointer; display:inline-flex; align-items:center; gap:5px; }
  button:disabled { opacity:.45; cursor:default; } button:focus-visible,textarea:focus-visible,select:focus-visible { outline:2px solid rgb(var(--color-primary)); outline-offset:2px; }
  footer { padding:10px 0; } .save { padding:7px 12px; border:1px solid rgb(var(--color-border)); border-radius:5px; }
</style>
