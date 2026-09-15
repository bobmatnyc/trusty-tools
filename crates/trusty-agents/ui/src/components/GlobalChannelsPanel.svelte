<script lang="ts">
  /**
   * Why (#7609 slice 6): slices 1-5 gave the harness-wide `[[channels]]` table a
   * model, a dispatch path and an HTTP surface, but the only way to change one
   * was `curl` or hand-editing `~/.trusty-agents/config.toml`. This is the
   * Global half of the Channels view's scope toggle.
   *
   * What: lists what `GET /api/channels` declares — provider, destination
   * (empty means account-wide), the three switches, and `route_to` as a
   * multi-select. Provider and destination are shown but not editable here:
   * both are validated per-adapter server-side, and slice 6's job is routing
   * and enablement, not authoring a new global channel. The whole channel
   * record round-trips, so a save never resets `transport`,
   * `poll_interval_secs`, the ingest/wake filters or the event types a
   * migration set.
   *
   * The route options are the UNION of `GET /api/agents` and the names the
   * channel already routes to, and that union is load-bearing (critic HIGH).
   * `/api/agents` is filtered to `role == "assistant" && !hidden`
   * (`api/server/projects.rs::apply_roster_filters`), while the server
   * validates `route_to` against the unfiltered dispatch roster
   * (`listeners::wake::candidate_agent_names`). A stored route to `pm`, `ctrl`
   * or a hidden assistant is therefore legal and absent from the catalog;
   * offering only the catalog would have let Svelte's multi-select binding —
   * which rebuilds the bound array from the CHECKED options alone — drop that
   * name on the first edit of any other route, and the next save would persist
   * the loss silently.
   * Test: `GlobalChannelsPanel.test.ts`.
   */
  import { onDestroy, onMount } from 'svelte';
  import { RefreshCw } from 'lucide-svelte';
  import { catalogAgents, fetchAgentCatalog } from '../stores/app';
  import {
    fetchGlobalChannels, saveGlobalChannels, channelErrorMessage, isChannelConflict,
    type GlobalChannelConfiguration, type GlobalChannel,
  } from '../lib/channels';
  /** Mirrors the assistant scope's contract so the parent can pin the view while an edit is pending. */
  export let dirty = false;
  export let saving = false;
  /** Raised after a save that actually landed, so the Assistant scope's
   * read-only routing panel can refresh instead of going stale (critic MEDIUM-3). */
  export let onSaved: () => void = () => {};
  let configuration: GlobalChannelConfiguration | null = null;
  let channels: GlobalChannel[] = [];
  let baseline = '[]';
  let loading = false, error = '', notice = '', catalogError = '', generation = 0;
  /**
   * Route names a channel arrived with that the catalog does not list, keyed by
   * channel id and SNAPSHOTTED at load rather than derived from the live
   * `route_to`. Derived, deselecting such a name would delete its own option
   * and the operator could never put it back.
   */
  let offCatalog: Record<string, string[]> = {};
  $: dirty = JSON.stringify(channels) !== baseline;
  $: assistantNames = $catalogAgents.map(agent => agent.name);
  /** Catalog names first, then this channel's own off-catalog routes. */
  const optionsFor = (id: string, names: string[]) =>
    [...names, ...(offCatalog[id] ?? []).filter(name => !names.includes(name))];
  function apply(next: GlobalChannelConfiguration) {
    configuration = next;
    channels = structuredClone(next.channels ?? []);
    baseline = JSON.stringify(channels);
    offCatalog = Object.fromEntries(channels.map(channel => [channel.id, [...channel.route_to]]));
  }
  /** Reload from the server; `false` when the read itself failed. */
  async function load(): Promise<boolean> {
    const token = ++generation;
    loading = true; error = ''; notice = '';
    try {
      const next = await fetchGlobalChannels();
      if (token !== generation) return false;
      apply(next);
      return true;
    } catch (cause) {
      if (token === generation) error = channelErrorMessage(cause);
      return false;
    } finally {
      if (token === generation) loading = false;
    }
  }
  async function save() {
    if (!configuration || saving || !dirty) return;
    const token = generation, revision = configuration.revision;
    saving = true; error = ''; notice = '';
    try {
      const result = await saveGlobalChannels(revision, channels);
      if (token !== generation) return;
      apply(result);
      notice = 'Channels saved.';
      onSaved();
    } catch (cause) {
      if (token !== generation) return;
      saving = false;
      // #7609: a lost compare-and-swap means the stored list is no longer the
      // one these edits were made against, so the only honest recovery is to
      // show what is actually stored.
      if (isChannelConflict(cause)) { if (await load()) error = 'Another writer changed the global channels. The list has been reloaded; make your change again and save.'; return; }
      error = channelErrorMessage(cause);
    } finally {
      if (token === generation) saving = false;
    }
  }
  // The catalog failing is reported, never swallowed: a panel that said "no
  // assistants are configured" because a fetch failed would invite the operator
  // to clear routes that are perfectly valid (critic MEDIUM-2).
  onMount(() => {
    void load();
    void fetchAgentCatalog().catch(cause => { catalogError = cause instanceof Error ? cause.message : String(cause); });
  });
  onDestroy(() => { generation++; });
</script>
<div class="global" aria-label="Global channels">
  <p class="muted">These channels belong to this host, not to one assistant. Each one fans its incoming updates out to the assistants selected below. An assistant's own channel for the same service and destination takes precedence over the global one.</p>
  {#if loading}<p role="status">Loading global channels…</p>{/if}
  {#if catalogError}<p class="error" role="status">The assistant list could not be read, so the routes below show only the names each channel already carries.</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}
  {#if configuration}
    {#if channels.length === 0}<p>No global channels are declared on this host.</p>{/if}
    <fieldset disabled={saving}>
      {#each channels as channel (channel.id)}
        {@const options = optionsFor(channel.id, assistantNames)}
        <article aria-label={`Global channel ${channel.name || channel.id}`}>
          <header><h3>{channel.name || channel.id}</h3><span class="muted">{channel.provider}{channel.target ? ` · ${channel.target}` : ' · account-wide'}</span></header>
          <div class="row">
            <label><input type="checkbox" aria-label={`${channel.id} enabled`} bind:checked={channel.enabled} />Enabled</label>
            <label><input type="checkbox" aria-label={`${channel.id} allow sending`} bind:checked={channel.send_enabled} disabled={!channel.target} />Allow sending</label>
            <label><input type="checkbox" aria-label={`${channel.id} receive updates`} bind:checked={channel.receive_enabled} />Receive updates</label>
          </div>
          {#if !channel.target}<p class="muted">No destination, so this channel can receive but not send.</p>{/if}
          <!-- #7609: the select is created only once its options exist, so the
               stored `route_to` is applied against real options rather than
               against an empty list the binding would silently drop. -->
          {#if options.length}
            <label class="routes">Route incoming updates to
              <select multiple size={Math.min(Math.max(options.length, 2), 6)} aria-label={`${channel.id} routes to`} bind:value={channel.route_to}>
                {#each options as name (name)}<option value={name}>{name}</option>{/each}
              </select>
            </label>
          {:else}<p class="muted">No assistants are configured on this host, so there is nothing to route to.</p>{/if}
          {#if !catalogError}
            {#each channel.route_to.filter(name => !assistantNames.includes(name)) as extra (extra)}
              <p class="muted" role="status">Routes to “{extra}”, which is not in the assistant catalog. It is still a valid route and is kept as it is.</p>
            {/each}
          {/if}
        </article>
      {/each}
    </fieldset>
    <div class="row">
      <button class="primary" on:click={save} disabled={!dirty || saving || loading}>{saving ? 'Saving…' : 'Save channels'}</button>
      <button on:click={load} disabled={saving || loading}><RefreshCw size={14} />{dirty ? 'Discard changes and reload' : 'Reload'}</button>
    </div>
  {/if}
</div>
<style>
  .global { font-size:13px; }
  .muted { color:rgb(var(--color-text-muted)); margin:8px 0; }
  .error { color:rgb(var(--color-warning)); }
  article { padding:16px; margin:16px 0; border:1px solid rgb(var(--color-border)); border-radius:10px; }
  header { display:flex; align-items:baseline; gap:12px; flex-wrap:wrap; }
  h3 { font-weight:600; }
  fieldset { border:0; padding:0; min-width:0; }
  .routes { display:block; margin-top:12px; }
  .routes select { display:block; margin-top:5px; min-width:220px; max-width:100%; border:1px solid rgb(var(--color-border)); border-radius:6px; padding:6px; background:rgb(var(--color-card-bg)); color:inherit; }
  .row { display:flex; gap:12px; align-items:center; flex-wrap:wrap; margin:10px 0; }
  label { display:flex; align-items:center; gap:6px; }
  button { display:inline-flex; align-items:center; gap:6px; border:1px solid rgb(var(--color-border)); border-radius:6px; padding:7px 10px; }
  button:disabled { opacity:.45; cursor:default; }
  .primary { background:rgb(var(--color-primary)); color:white; }
</style>
