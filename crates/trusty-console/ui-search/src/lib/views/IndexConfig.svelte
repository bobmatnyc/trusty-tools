<script>
  /*
   * Why: Each index needs tunable indexing-hygiene settings (issue #1372) —
   * which directories to skip, how large a "data" file may be before it's
   * excluded, the extension allow-list, exclude globs, and doc/gitignore
   * toggles. This drill-down (reached from the Indexes table) mirrors
   * Config.svelte's load→edit→dirty→save pattern but is scoped to one index
   * and surfaces a reindex prompt when the daemon reports `reindex_required`.
   * What: Loads `GET /indexes/:id/config`, lets the operator edit the fields,
   * tracks a dirty flag, PATCHes only on Save, then — if the daemon says a
   * reindex is required — offers a "Reindex now" action wired to the existing
   * reindex API.
   * Test: open #/indexes/<id>/config, toggle include_docs, click Save, accept
   * the PATCH, observe the "reindex required" notice and that "Reindex now"
   * fires POST /indexes/<id>/reindex.
   */
  import { onMount } from 'svelte';
  import { api } from '../api.js';
  import { navigate } from '../router.svelte.js';
  import TagListInput from '../components/TagListInput.svelte';

  let { id } = $props();

  // Live config from the daemon and the editable draft fields.
  let config = $state(null);
  let loadError = $state(null);
  let saving = $state(false);
  let saveError = $state(null);
  let saveMessage = $state(null);
  let reindexRequired = $state(false);
  let reindexing = $state(false);
  let reindexMessage = $state(null);

  // Draft fields. Lists are arrays; toggles booleans; the size cap is split
  // into a numeric value + a unit so we can present KB/MB but store bytes.
  let extraSkipDirs = $state([]);
  let extensions = $state([]);
  let excludeGlobs = $state([]);
  let includeDocs = $state(true);
  let respectGitignore = $state(true);
  let sizeValue = $state(''); // string draft for the numeric size input
  let sizeUnit = $state('KB'); // 'B' | 'KB' | 'MB'

  const UNIT_BYTES = { B: 1, KB: 1024, MB: 1024 * 1024 };

  onMount(loadConfig);

  /**
   * Why: choose the friendliest unit for an existing byte count so the input
   * isn't always in raw bytes.
   * What: returns { value, unit } picking MB if it divides evenly into MB,
   * else KB if it divides evenly into KB, else raw bytes.
   * Test: bytesToParts(65536) → { value: 64, unit: 'KB' };
   * bytesToParts(1048576) → { value: 1, unit: 'MB' }.
   */
  function bytesToParts(bytes) {
    if (typeof bytes !== 'number' || bytes <= 0) return { value: '', unit: 'KB' };
    if (bytes % UNIT_BYTES.MB === 0) return { value: bytes / UNIT_BYTES.MB, unit: 'MB' };
    if (bytes % UNIT_BYTES.KB === 0) return { value: bytes / UNIT_BYTES.KB, unit: 'KB' };
    return { value: bytes, unit: 'B' };
  }

  function hydrate(c) {
    config = c;
    extraSkipDirs = [...(c.extra_skip_dirs ?? [])];
    extensions = [...(c.extensions ?? [])];
    excludeGlobs = [...(c.exclude_globs ?? [])];
    includeDocs = Boolean(c.include_docs);
    respectGitignore = Boolean(c.respect_gitignore);
    const parts = bytesToParts(c.data_file_max_bytes);
    sizeValue = parts.value === '' ? '' : String(parts.value);
    sizeUnit = parts.unit;
  }

  async function loadConfig() {
    loadError = null;
    try {
      hydrate(await api.getIndexConfig(id));
    } catch (e) {
      loadError = e.message || String(e);
    }
  }

  // The current byte value implied by the draft size input (or null if blank).
  let draftBytes = $derived.by(() => {
    const t = sizeValue.trim();
    if (t === '') return null;
    const n = Number(t);
    if (!Number.isFinite(n)) return NaN;
    return Math.round(n * UNIT_BYTES[sizeUnit]);
  });

  // Compare arrays order-insensitively for dirty detection. Order-independence
  // is intentional: the walker treats extra_skip_dirs / extensions /
  // exclude_globs as sets (membership tests, no positional meaning), so a pure
  // reorder is not a semantic change and should not mark the form dirty.
  function sameSet(a, b) {
    if (a.length !== b.length) return false;
    const sb = new Set(b);
    return a.every((x) => sb.has(x));
  }

  let dirty = $derived.by(() => {
    if (config == null) return false;
    if (!sameSet(extraSkipDirs, config.extra_skip_dirs ?? [])) return true;
    if (!sameSet(extensions, config.extensions ?? [])) return true;
    if (!sameSet(excludeGlobs, config.exclude_globs ?? [])) return true;
    if (includeDocs !== Boolean(config.include_docs)) return true;
    if (respectGitignore !== Boolean(config.respect_gitignore)) return true;
    // Size: blank means "leave unchanged" (omit), so only dirty when a valid
    // positive byte value differs from the saved one.
    if (draftBytes != null && !Number.isNaN(draftBytes) && draftBytes > 0) {
      if (draftBytes !== config.data_file_max_bytes) return true;
    }
    return false;
  });

  /**
   * Why: PATCH only the fields the operator actually changed; the size cap is
   * omitted when blank (leave unchanged) and rejected when ≤ 0 (the daemon
   * 400s on 0).
   * What: builds the JSON patch body from the dirty draft, throwing on an
   * invalid size value so the caller can surface it.
   * Test: blank size → no `data_file_max_bytes` key; "0" → throws; "32" KB →
   * { data_file_max_bytes: 32768 }.
   */
  function buildPatch() {
    const patch = {};
    if (!sameSet(extraSkipDirs, config.extra_skip_dirs ?? [])) {
      patch.extra_skip_dirs = extraSkipDirs;
    }
    if (!sameSet(extensions, config.extensions ?? [])) {
      patch.extensions = extensions;
    }
    if (!sameSet(excludeGlobs, config.exclude_globs ?? [])) {
      patch.exclude_globs = excludeGlobs;
    }
    if (includeDocs !== Boolean(config.include_docs)) {
      patch.include_docs = includeDocs;
    }
    if (respectGitignore !== Boolean(config.respect_gitignore)) {
      patch.respect_gitignore = respectGitignore;
    }
    const t = sizeValue.trim();
    if (t !== '') {
      if (Number.isNaN(draftBytes) || draftBytes <= 0) {
        throw new Error('Data-file size cap must be a positive number.');
      }
      if (draftBytes !== config.data_file_max_bytes) {
        patch.data_file_max_bytes = draftBytes;
      }
    }
    return patch;
  }

  async function save() {
    saveError = null;
    saveMessage = null;
    let patch;
    try {
      patch = buildPatch();
    } catch (e) {
      saveError = e.message;
      return;
    }
    if (Object.keys(patch).length === 0) {
      saveMessage = 'No changes to save.';
      return;
    }
    saving = true;
    try {
      const res = await api.updateIndexConfig(id, patch);
      hydrate(res.config);
      reindexRequired = Boolean(res.reindex_required);
      saveMessage = 'Settings saved.';
    } catch (e) {
      saveError = e.message || String(e);
    } finally {
      saving = false;
    }
  }

  function reset() {
    if (config) hydrate(config);
    saveError = null;
    saveMessage = null;
  }

  async function reindexNow() {
    reindexing = true;
    reindexMessage = null;
    try {
      await api.reindex(id);
      reindexRequired = false;
      reindexMessage = 'Reindex queued. Watch progress on the Indexes page.';
    } catch (e) {
      reindexMessage = `Reindex failed: ${e.message || e}`;
    } finally {
      reindexing = false;
    }
  }
</script>

<div class="crumbs">
  <a
    href="#/indexes"
    onclick={(e) => {
      e.preventDefault();
      navigate('/indexes');
    }}>Indexes</a
  >
  <span class="sep">/</span>
  <span class="text-mono">{id}</span>
  <span class="sep">/</span>
  <span>Settings</span>
</div>

<h1 class="page-title">Index settings — <span class="text-mono">{id}</span></h1>

{#if loadError}
  <div class="card" style="border-color: var(--trusty-danger)">
    <div class="card-header" style="color: var(--trusty-danger)">Failed to load config</div>
    <div class="card-body">
      <p>{loadError}</p>
      <button class="btn" onclick={loadConfig}>Retry</button>
    </div>
  </div>
{:else if config == null}
  <div class="card"><div class="card-body text-muted">Loading current settings…</div></div>
{:else}
  {#if reindexRequired}
    <div class="card notice mb-4">
      <div class="card-body flex-between">
        <span>
          <strong>Reindex required.</strong> These changes affect what gets indexed and
          only take effect after a reindex.
        </span>
        <button class="btn btn-primary" disabled={reindexing} onclick={reindexNow}>
          {reindexing ? 'Queuing…' : 'Reindex now'}
        </button>
      </div>
    </div>
  {/if}
  {#if reindexMessage}
    <p class="text-sm mb-3" style="color: var(--trusty-text-secondary)">{reindexMessage}</p>
  {/if}

  <div class="card mb-4">
    <div class="card-header flex-between">
      <span>Indexing hygiene</span>
      {#if dirty}<span class="badge badge-warning">unsaved changes</span>{/if}
    </div>
    <div class="card-body">
      {#if saveError}
        <p class="text-sm mb-3" style="color: var(--trusty-danger)">{saveError}</p>
      {/if}
      {#if saveMessage}
        <p class="text-sm mb-3" style="color: var(--trusty-success)">{saveMessage}</p>
      {/if}

      <div class="form-group">
        <label class="form-label" for="size-cap">Data-file size cap</label>
        <div class="size-row">
          <input
            id="size-cap"
            type="text"
            inputmode="numeric"
            class="input size-input"
            placeholder="leave blank to keep"
            bind:value={sizeValue}
          />
          <select class="input unit-select" bind:value={sizeUnit}>
            <option value="B">B</option>
            <option value="KB">KB</option>
            <option value="MB">MB</option>
          </select>
        </div>
        <div class="field-help">
          Files larger than this with a data extension (JSON/XML/TXT/LOG) are skipped.
          {#if draftBytes != null && Number.isFinite(draftBytes) && draftBytes > 0}
            = <code>{draftBytes.toLocaleString()}</code> bytes.
          {:else if config.data_file_max_bytes}
            Current: <code>{config.data_file_max_bytes.toLocaleString()}</code> bytes.
          {/if}
        </div>
      </div>

      <TagListInput
        id="skip-dirs"
        label="Extra skip directories"
        help="One directory basename per entry (e.g. data, exports). These are skipped in addition to the built-in defaults."
        placeholder="e.g. data"
        bind:items={extraSkipDirs}
      />

      <TagListInput
        id="extensions"
        label="Extensions allow-list"
        help="Leading dot optional. Empty = index all default source extensions."
        placeholder="e.g. rs"
        bind:items={extensions}
      />

      <TagListInput
        id="exclude-globs"
        label="Exclude globs"
        help="Glob patterns to exclude (e.g. **/gen/**)."
        placeholder="e.g. **/gen/**"
        bind:items={excludeGlobs}
      />

      <div class="form-group toggle-row">
        <label class="toggle">
          <input type="checkbox" bind:checked={includeDocs} />
          <span>Include documentation files</span>
        </label>
        <label class="toggle">
          <input type="checkbox" bind:checked={respectGitignore} />
          <span>Respect <code>.gitignore</code></span>
        </label>
      </div>

      <div class="flex-gap-2">
        <button class="btn btn-primary" disabled={!dirty || saving} onclick={save}>
          {saving ? 'Saving…' : 'Save changes'}
        </button>
        <button class="btn" disabled={!dirty || saving} onclick={reset}>Reset</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .page-title {
    font-size: var(--trusty-fs-xl);
    margin: 0 0 var(--trusty-space-5) 0;
    font-weight: 600;
  }
  .crumbs {
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-text-muted);
    margin-bottom: var(--trusty-space-3);
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .crumbs .sep {
    color: var(--trusty-border);
  }
  .notice {
    border-color: var(--trusty-warning);
    background: var(--trusty-warning-soft);
  }
  .size-row {
    display: flex;
    gap: var(--trusty-space-2);
    align-items: stretch;
  }
  .size-input {
    max-width: 200px;
  }
  .unit-select {
    max-width: 90px;
  }
  .field-help {
    margin-top: var(--trusty-space-2);
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-muted);
  }
  .toggle-row {
    display: flex;
    flex-wrap: wrap;
    gap: var(--trusty-space-5);
  }
  .toggle {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    font-size: var(--trusty-fs-sm);
    cursor: pointer;
  }
  .toggle input {
    width: 15px;
    height: 15px;
    accent-color: var(--trusty-primary, #3b82f6);
    cursor: pointer;
  }
</style>
