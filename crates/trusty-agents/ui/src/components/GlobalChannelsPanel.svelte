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
   *
   * #8187: the editor also CREATES and DELETES. Both go through the
   * per-channel routes rather than the whole-list PUT, so a create races
   * another writer's unrelated edit into a 409 instead of republishing a stale
   * list, and a delete can be refused by a server that knows something this
   * page does not — which per-assistant bindings overlay the channel. Creating
   * and deleting are deliberately not part of the Save above: both publish on
   * their own revision, so leaving them pending beside an unsaved field edit
   * would mean two drafts of one list.
   *
   * #8187 (critic HIGH): a delete removes the DECLARATION, and whether a
   * receiver for it keeps running until the daemon restarts is the server's to
   * report — no field this page holds predicts it. So the confirmation claims
   * nothing about when receiving stops, and the notice after it says the
   * restart fact exactly when `receiving_until_restart` came back true.
   * Test: `GlobalChannelsPanel.test.ts`.
   */
  import { onDestroy, onMount, tick } from 'svelte';
  import { Plus, RefreshCw, Trash2 } from 'lucide-svelte';
  import { catalogAgents, fetchAgentCatalog } from '../stores/app';
  import {
    fetchGlobalChannels, saveGlobalChannels, createGlobalChannel, deleteGlobalChannel,
    channelErrorMessage, isChannelConflict, channelReferences, offerableProviders,
    type GlobalChannelConfiguration, type GlobalChannel, type NewGlobalChannel,
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
  /** The channel being authored, or null when the Add form is closed (#8187). */
  let draft: NewGlobalChannel | null = null;
  /** The channel the operator asked to delete, and what the server said about it. */
  let pending: GlobalChannel | null = null;
  let referencedBy: string[] = [];
  let draftError = '', pendingError = '', busy = false;
  /** The Delete button that opened the confirmation, so focus can go back to it. */
  let invoker: HTMLElement | null = null;
  /** Where focus goes when the row that raised the confirmation is deleted (#8187). */
  let addButton: HTMLButtonElement | null = null;
  /** The confirmation's sheet, so Tab can be kept inside it (#8187). */
  let sheet: HTMLElement | null = null;
  /** The confirmation's Cancel button — where focus goes when the force appears (#8187). */
  let cancelDelete: HTMLButtonElement | null = null;
  /** The panel's own root, the focus target of last resort (#8187). */
  let panelRoot: HTMLElement | null = null;
  /**
   * False while the parent's scope toggle hides this panel (#8187).
   *
   * Why: the toggle hides the panel with `display:none` on an ancestor
   * (`ChannelsView.svelte`), which does not unmount it. An open confirmation
   * hidden that way stayed ARMED — the window Escape handler still answered for
   * it and `dirty` still pinned the assistant selector — with nothing on screen
   * to explain either (critic MEDIUM).
   */
  export let visible = true;
  /**
   * An open Add form or delete confirmation counts as an unsaved edit: the
   * parent pins the assistant selector on this flag, and the list Save must not
   * publish a revision a create or delete is about to consume.
   */
  $: listDirty = JSON.stringify(channels) !== baseline;
  $: dirty = draft !== null || pending !== null || listDirty;
  /**
   * The stored list is read-only while either editor is open (critic HIGH).
   *
   * Why: both writes finish by calling `apply`, which REPLACES `channels`. A
   * field edit made after the Add form opened would be thrown away by the
   * create that follows it, under a notice announcing a success.
   */
  $: listLocked = saving || draft !== null || pending !== null;
  // #8187 (critic MEDIUM-1): hidden and armed is the state to end, but never
  // while the DELETE is in flight — see `abandonHidden`.
  $: if (!visible && pending && !busy) void abandonHidden();
  $: assistantNames = $catalogAgents.map(agent => agent.name);
  // #8187: the daemon's own provider table, minus the test-only adapters, and
  // the dispatch roster the server validates `route_to` against — which is NOT
  // the assistant catalog and may name `pm`, `ctrl` or a hidden assistant.
  $: providerOptions = offerableProviders(configuration?.providers);
  $: routable = configuration?.routable_assistants ?? assistantNames;
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
    // #8187 (critic LOW): `listDirty`, not `dirty` — the button's own guard. An
    // open editor makes `dirty` true with nothing for this PUT to publish.
    if (!configuration || saving || !listDirty) return;
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
      if (isChannelConflict(cause)) { if (await load()) error = RELOADED; return; }
      error = channelErrorMessage(cause);
    } finally {
      if (token === generation) saving = false;
    }
  }
  /** Close both editors and show what is actually stored. */
  function reload() { draft = null; draftError = ''; void closeDelete(); void load(); }
  /** One sentence for every lost compare-and-swap, in all three writes (#8187). */
  const RELOADED = 'Another writer changed the global channels. The list has been reloaded; make your change again and save.';
  const DUPLICATE = 'A global channel with this ID already exists. Give this one a different ID.';
  /**
   * What a create or delete says when the list under it has unsaved changes.
   *
   * Why the buttons are not enough (critic HIGH): a change event already in
   * flight when an editor opened still reaches the binding, so both writes
   * re-check immediately before going to the wire.
   */
  const UNSAVED = 'The list below has unsaved changes. Save or discard them first — adding or deleting publishes the stored list, which would throw those changes away.';
  // #8187 (critic HIGH): said after a delete, and only when its response
  // reported `receiving_until_restart`.
  const RECEIVER_LIVES = 'Its receiver keeps polling until the daemon restarts. Restarting it stops the receiver.';
  /** Open the Add form on a channel that does nothing until it is enabled. */
  function beginAdd() {
    const provider = providerOptions[0];
    if (!provider) return;
    draftError = ''; error = ''; notice = '';
    // #8187: every switch starts off. A channel created enabled would begin
    // polling — or be addressable — before the operator has given it a
    // destination or checked where it routes.
    draft = {
      id: '', name: '', provider: provider.id, target: '',
      enabled: false, send_enabled: false, receive_enabled: false,
      instructions: '', event_types: [], route_to: [],
      ingest_filter: { label_ids: [] },
      wake_filter: { from: [], include_labels: [], exclude_labels: [], subject_contains: [], snippet_contains: [] },
    };
  }
  /**
   * `POST /api/channels` under the revision this list was read at.
   *
   * A 409 is either a duplicate id or a lost compare-and-swap, and the server
   * answers both the same way; the reloaded list is what tells them apart, so
   * the draft is kept either way and the operator retries rather than retypes.
   */
  async function create() {
    // #8187 (critic LOW): `loading` too — `busy` clears before the reload that
    // follows a 409, and a second POST then went out at the losing revision.
    if (!configuration || !draft || busy || loading) return;
    if (listDirty) { draftError = UNSAVED; return; }
    const id = draft.id.trim(), name = draft.name.trim() || id;
    if (!id) { draftError = 'Give the channel an ID.'; return; }
    if (channels.some(channel => channel.id === id)) { draftError = DUPLICATE; return; }
    const token = generation, revision = configuration.revision;
    busy = true; draftError = ''; error = ''; notice = '';
    try {
      const result = await createGlobalChannel(revision, { ...draft, id, name });
      if (token !== generation) return;
      apply(result); draft = null; notice = `${name} added. It is switched off until you enable it and save.`;
      onSaved();
    } catch (cause) {
      if (token !== generation) return;
      if (isChannelConflict(cause)) {
        busy = false;
        // The reload is what tells a duplicate id from a lost race; when it
        // fails too, the create's own refusal is all there is to say about it
        // (critic LOW).
        if (!(await load())) { draftError = channelErrorMessage(cause); return; }
        if (channels.some(channel => channel.id === id)) draftError = DUPLICATE; else error = RELOADED;
        return;
      }
      draftError = channelErrorMessage(cause);
    } finally {
      if (token === generation) busy = false;
    }
  }
  /** `source` is the button clicked, kept so closing can give focus back to it. */
  function beginDelete(channel: GlobalChannel, source: HTMLElement | null) {
    invoker = source;
    pending = channel; referencedBy = []; pendingError = ''; error = ''; notice = '';
  }
  /**
   * Dismiss the confirmation and give focus to `target`, the Delete button that
   * raised it unless a caller names another (critic MEDIUM-2, LOW).
   *
   * What: the focus call waits a tick. The list `fieldset` is disabled while
   * the dialog is up, and a disabled button cannot take focus, so restoring it
   * before the DOM re-renders would silently land on `<body>`. A landed delete
   * passes the Add button, because its own row — and the button in it — is gone.
   */
  async function closeDelete(target: HTMLElement | null = invoker) {
    pending = null; referencedBy = []; pendingError = '';
    invoker = null;
    await tick();
    // #8187 (critic LOW): a disabled target cannot take focus — Add channel is
    // disabled outright when the daemon offers no provider — so focus falls to
    // the panel rather than out of it to `<body>`.
    const usable = target && !(target as HTMLButtonElement).disabled ? target : panelRoot;
    usable?.focus();
  }
  /** What a delete refused for live overlays says once its sheet is gone. */
  const refusedBy = (names: string[]) =>
    `${names.join(', ')} ${names.length === 1 ? 'still binds' : 'still bind'} this channel, so it was not deleted. Start the delete again to be offered the force.`;
  /**
   * Close a confirmation the scope toggle hid, carrying its outcome up.
   *
   * Why: the sheet is the only surface that reports a DELETE's outcome, so this
   * runs only once `busy` has cleared — an in-flight request is never orphaned
   * by a visibility change — and whatever the sheet was left showing becomes a
   * panel-level error rather than disappearing with it.
   * Test: `GlobalChannelsPanel.test.ts::shows a refusal that landed while the panel was hidden`.
   */
  async function abandonHidden() {
    const refusal = pendingError || (referencedBy.length ? refusedBy(referencedBy) : '');
    await closeDelete();
    if (refusal) error = refusal;
  }
  /**
   * The confirmation claims `aria-modal`, so Tab stays inside it (critic LOW).
   *
   * What: the sheet's own enabled buttons are the whole tab ring. Focus leaving
   * either end — or sitting outside the sheet entirely, which is where it lands
   * after the Delete button behind the backdrop is disabled — wraps back in. An
   * EMPTY ring is the in-flight state, where every button is disabled; Tab then
   * holds on the sheet itself rather than walking out (critic LOW).
   */
  function trapTab(event: KeyboardEvent) {
    const ring = sheet ? [...sheet.querySelectorAll<HTMLButtonElement>('button:not([disabled])')] : [];
    if (!ring.length) { event.preventDefault(); sheet?.focus(); return; }
    const first = ring[0], last = ring[ring.length - 1], here = document.activeElement;
    const inside = sheet?.contains(here) ?? false;
    if (inside && here !== (event.shiftKey ? first : last)) return;
    event.preventDefault();
    (event.shiftKey ? last : first).focus();
  }
  /** Escape dismisses the confirmation wherever focus is — never mid-write. */
  function onWindowKeydown(event: KeyboardEvent) {
    if (!pending) return;
    if (event.key === 'Tab') { trapTab(event); return; }
    if (event.key !== 'Escape' || busy) return;
    event.preventDefault();
    void closeDelete();
  }
  /**
   * `DELETE /api/channels/{id}`, forced only after the server has named what
   * forcing costs.
   *
   * Why the two-step: the assistants whose bindings overlay this channel are
   * not knowable from this page — they live in each assistant's own file — so
   * the first delete asks the server, and only a refusal naming them offers the
   * force. `force` is never sent on the operator's first click.
   */
  async function confirmDelete(force: boolean) {
    if (!configuration || !pending || busy) return;
    if (listDirty) { pendingError = UNSAVED; return; }
    const token = generation, revision = configuration.revision, victim = pending;
    const label = victim.name || victim.id;
    busy = true; pendingError = ''; error = ''; notice = '';
    try {
      const result = await deleteGlobalChannel(revision, victim.id, force);
      if (token !== generation) return;
      apply(result);
      // #8187 (critic LOW): the row's own Delete button is gone with the row,
      // so focus goes somewhere that still exists rather than to a detached node.
      void closeDelete(addButton ?? null);
      const inert = result.inert_bindings ?? [];
      const said = [`${label} deleted.`];
      if (inert.length) said.push(`${inert.join(', ')} still carry a binding for it; those bindings now address nothing until they are removed.`);
      // #8187 (critic HIGH): the route removed the declaration; it cannot stop a
      // poll loop already running, and only the server knows one was.
      if (result.receiving_until_restart) said.push(RECEIVER_LIVES);
      notice = said.join(' ');
      onSaved();
    } catch (cause) {
      if (token !== generation) return;
      const named = channelReferences(cause);
      if (named) {
        // #8187 (critic MEDIUM-2): the force button takes the refused one's
        // place, so focus leaves it — a repeated or held Enter must not force a
        // delete over a warning that has not been read. `busy` clears first
        // because a disabled button cannot take the focus.
        referencedBy = named; busy = false;
        await tick();
        cancelDelete?.focus();
        return;
      }
      if (isChannelConflict(cause)) {
        busy = false; void closeDelete();
        if (await load()) error = RELOADED;
        return;
      }
      pendingError = channelErrorMessage(cause);
    } finally {
      if (token === generation) busy = false;
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
  /** Give the confirmation the focus on open, so Escape and Tab act on IT. */
  const focusOnMount = (node: HTMLElement) => { node.focus(); };
</script>
<svelte:window on:keydown={onWindowKeydown} />
<div class="global" aria-label="Global channels" tabindex="-1" bind:this={panelRoot}>
  <p class="muted">These channels belong to this host, not to one assistant. Each one fans its incoming updates out to the assistants selected below. An assistant's own channel for the same service and destination takes precedence over the global one.</p>
  {#if loading}<p role="status">Loading global channels…</p>{/if}
  {#if catalogError}<p class="error" role="status">The assistant list could not be read, so the routes below show only the names each channel already carries.</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}
  {#if configuration}
    {#if channels.length === 0}<p>No global channels are declared on this host.</p>{/if}
    <fieldset disabled={listLocked}>
      {#each channels as channel (channel.id)}
        {@const options = optionsFor(channel.id, assistantNames)}
        <article aria-label={`Global channel ${channel.name || channel.id}`}>
          <header><h3>{channel.name || channel.id}</h3><span class="muted">{channel.provider}{channel.target ? ` · ${channel.target}` : ' · account-wide'}</span>
            <!-- #8187: a delete publishes the STORED list without this channel,
                 so an unsaved field edit would be thrown away by it rather than
                 carried along. Save or discard first. -->
            <button class="delete" aria-label={`Delete ${channel.name || channel.id}`} on:click={event => beginDelete(channel, event.currentTarget)} disabled={busy || loading || listDirty}><Trash2 size={14} />Delete</button>
          </header>
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
    {#if draft}
      <!-- #8187: the provider list is the daemon's, never a list written here,
           and the route names are the roster the server validates against. -->
      <div class="draft" role="group" aria-label="New global channel">
        <h3>New global channel</h3>
        <label>ID<input aria-label="New channel ID" bind:value={draft.id} maxlength="64" placeholder="ops-slack" /></label>
        <label>Name<input aria-label="New channel name" bind:value={draft.name} maxlength="128" placeholder="Ops alerts" /></label>
        <label>Service
          <select aria-label="New channel provider" bind:value={draft.provider}>
            {#each providerOptions as provider (provider.id)}<option value={provider.id}>{provider.name}</option>{/each}
          </select>
        </label>
        {#if routable.length}
          <label>Route incoming updates to
            <select multiple size={Math.min(Math.max(routable.length, 2), 6)} aria-label="New channel routes to" bind:value={draft.route_to}>
              {#each routable as name (name)}<option value={name}>{name}</option>{/each}
            </select>
          </label>
        {:else}<p class="muted">No assistants are configured on this host, so there is nothing to route to yet.</p>{/if}
        <!-- #8187 (critic MEDIUM): this panel has no destination control, so
             the copy names where a destination is actually set. -->
        <p class="muted">A new channel starts switched off with no destination, so it neither polls nor sends. A destination is set in this host's <code>config.toml</code>; enable the channel here once it has one.</p>
        {#if draftError}<p class="error" role="alert">{draftError}</p>{/if}
        <div class="row">
          <button class="primary" on:click={() => create()} disabled={busy || loading}>{busy ? 'Creating…' : 'Create channel'}</button>
          <button on:click={() => { draft = null; draftError = ''; }} disabled={busy}>Cancel</button>
        </div>
      </div>
    {/if}
    <div class="row">
      <button bind:this={addButton} on:click={beginAdd} disabled={busy || saving || loading || listDirty || draft !== null || providerOptions.length === 0}><Plus size={14} />Add channel</button>
      <button class="primary" on:click={save} disabled={!listDirty || saving || loading || busy}>{saving ? 'Saving…' : 'Save channels'}</button>
      <button on:click={reload} disabled={saving || loading || busy}><RefreshCw size={14} />{dirty ? 'Discard changes and reload' : 'Reload'}</button>
    </div>
    {#if listDirty}<p class="muted">Save or discard these changes before adding or deleting a channel.</p>{/if}
  {/if}
</div>
{#if pending}
  <!-- #8187: the same confirmation shape ProjectsView's modal uses — a backdrop
       and a sheet, no dialog library. `referencedBy` is what the server refused
       with, so the consequence named here is the one it measured. Escape is
       handled on the window, not here: this element never holds the focus, and
       a handler on it only fired for a click that had already landed on it. -->
  <div class="backdrop" role="dialog" aria-modal="true" aria-label="Delete global channel">
    <div class="sheet" bind:this={sheet} tabindex="-1">
      <h3>Delete “{pending.name || pending.id}”?</h3>
      {#if referencedBy.length}
        <p role="alert">{referencedBy.join(', ')} {referencedBy.length === 1 ? 'still binds' : 'still bind'} this channel.</p>
        <p>Deleting it anyway keeps {referencedBy.length === 1 ? 'that binding' : 'those bindings'} on the assistant, but inert: {referencedBy.length === 1 ? 'it addresses' : 'they address'} nothing, send nothing and receive nothing until {referencedBy.length === 1 ? 'it is' : 'they are'} removed there. Removing the binding first avoids that.</p>
      {:else}
        <!-- #8187 (critic HIGH): whether a receiver outlives the delete is the
             server's to report and no client-side field predicts it, so this
             says nothing in either direction until the response does. -->
        <p>This removes the channel from this host and is not covered by Save. Anything it routes to stops receiving its updates. The result will say whether a receiver for it is left running until the daemon restarts.</p>
      {/if}
      {#if pendingError}<p class="error" role="alert">{pendingError}</p>{/if}
      <div class="row">
        <button class="danger" on:click={() => confirmDelete(referencedBy.length > 0)} disabled={busy}>{busy ? 'Deleting…' : referencedBy.length ? 'Delete anyway' : 'Delete channel'}</button>
        <button bind:this={cancelDelete} use:focusOnMount on:click={() => closeDelete()} disabled={busy}>Cancel</button>
      </div>
    </div>
  </div>
{/if}
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
  .delete { margin-left:auto; }
  .danger { background:rgb(var(--color-warning)); color:white; }
  .draft { padding:16px; margin:16px 0; border:1px solid rgb(var(--color-border)); border-radius:10px; }
  .draft label { display:block; margin-top:12px; }
  .draft input, .draft select { display:block; margin-top:5px; min-width:220px; max-width:100%; border:1px solid rgb(var(--color-border)); border-radius:6px; padding:6px; background:rgb(var(--color-card-bg)); color:inherit; }
  .backdrop { position:fixed; inset:0; z-index:50; display:flex; align-items:center; justify-content:center; background:rgba(0,0,0,.5); }
  .sheet { max-width:480px; padding:20px; border:1px solid rgb(var(--color-border)); border-radius:10px; background:rgb(var(--color-card-bg)); font-size:13px; }
  .sheet p { margin:10px 0; }
</style>
