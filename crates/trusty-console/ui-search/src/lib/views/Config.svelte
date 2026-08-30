<script>
  /*
   * Why: Operators need a single pane to verify and tune the daemon's
   * configuration. The redesign (issue #38) makes the memory-limit fields
   * editable with a Save button → `PATCH /config`, a current-vs-pending diff
   * highlight, and a confirmation dialog so a misclick can't drop the limit.
   * What: Read-only daemon-detail table plus an editable memory-limits form.
   * Pending edits are highlighted; Save asks for confirmation then PATCHes.
   * Test: open #/config, change the memory limit, confirm the field turns
   * amber, click Save, accept the dialog, observe the new value persist.
   */
  import { onMount } from 'svelte';
  import { api } from '../api.js';
  import { getHealth, getIndexes } from '../state.svelte.js';

  let health = $derived(getHealth());
  let indexes = $derived(getIndexes());

  let totalChunks = $derived(
    indexes.reduce((sum, ix) => sum + (ix.chunk_count || 0), 0)
  );

  let openrouterEnabled = $derived(
    typeof window !== 'undefined' && !!window.__OPENROUTER_ENABLED__
  );
  let daemonPort = $derived(
    (typeof window !== 'undefined' && window.__DAEMON_PORT__) || null
  );

  // Live config from the daemon, and the editable draft.
  let config = $state(null);
  let configError = $state(null);
  let saving = $state(false);
  let saveMessage = $state(null);

  // Draft fields are strings so an empty input maps cleanly to "unlimited".
  let memoryLimit = $state('');
  let indexMemoryLimit = $state('');

  onMount(loadConfig);

  async function loadConfig() {
    try {
      config = await api.getConfig();
      memoryLimit = config.memory_limit_mb == null ? '' : String(config.memory_limit_mb);
      indexMemoryLimit =
        config.index_memory_limit_mb == null ? '' : String(config.index_memory_limit_mb);
      configError = null;
    } catch (e) {
      configError = e.message || String(e);
    }
  }

  // A field is "dirty" when its draft differs from the saved config value.
  let memoryDirty = $derived(
    config != null &&
      memoryLimit.trim() !== (config.memory_limit_mb == null ? '' : String(config.memory_limit_mb))
  );
  let indexMemoryDirty = $derived(
    config != null &&
      indexMemoryLimit.trim() !==
        (config.index_memory_limit_mb == null ? '' : String(config.index_memory_limit_mb))
  );
  let dirty = $derived(memoryDirty || indexMemoryDirty);

  /**
   * Why: PATCH semantics distinguish "leave unchanged" (omit) from "disable"
   * (explicit null) from "set" (number). We build the patch from dirty fields
   * only so an untouched field is never sent.
   * What: returns the JSON patch body, validating numeric input.
   * Test: empty string → null, "2048" → 2048, "abc" → throws.
   */
  function buildPatch() {
    const patch = {};
    const parse = (raw, label) => {
      const t = raw.trim();
      if (t === '') return null; // disable the limit
      const n = Number(t);
      if (!Number.isInteger(n) || n <= 0) {
        throw new Error(`${label} must be a positive integer (MB) or blank for unlimited`);
      }
      return n;
    };
    if (memoryDirty) patch.memory_limit_mb = parse(memoryLimit, 'Memory limit');
    if (indexMemoryDirty) patch.index_memory_limit_mb = parse(indexMemoryLimit, 'Index memory limit');
    return patch;
  }

  async function save() {
    saveMessage = null;
    let patch;
    try {
      patch = buildPatch();
    } catch (e) {
      configError = e.message;
      return;
    }
    const summary = Object.entries(patch)
      .map(([k, v]) => `${k} → ${v == null ? 'unlimited' : v + ' MB'}`)
      .join('\n');
    if (!confirm(`Apply these configuration changes?\n\n${summary}`)) return;
    saving = true;
    configError = null;
    try {
      config = await api.updateConfig(patch);
      memoryLimit = config.memory_limit_mb == null ? '' : String(config.memory_limit_mb);
      indexMemoryLimit =
        config.index_memory_limit_mb == null ? '' : String(config.index_memory_limit_mb);
      saveMessage = 'Configuration saved.';
    } catch (e) {
      configError = e.message || String(e);
    } finally {
      saving = false;
    }
  }

  function reset() {
    if (!config) return;
    memoryLimit = config.memory_limit_mb == null ? '' : String(config.memory_limit_mb);
    indexMemoryLimit =
      config.index_memory_limit_mb == null ? '' : String(config.index_memory_limit_mb);
    saveMessage = null;
    configError = null;
  }

  /**
   * Why: reuse the dashboard's uptime humaniser so both panes agree.
   * What: "Xs / Xm / Xh / Xd" or "—".
   * Test: humanUptime(3600) === "1h".
   */
  function humanUptime(secs) {
    if (typeof secs !== 'number' || secs < 0) return '—';
    if (secs < 60) return `${secs}s`;
    const m = Math.floor(secs / 60);
    if (m < 60) return `${m}m`;
    const h = Math.floor(m / 60);
    if (h < 24) return `${h}h`;
    return `${Math.floor(h / 24)}d`;
  }
</script>

<h1 class="page-title">Configuration</h1>

<div class="stat-grid">
  <div class="stat">
    <div class="stat-label">Status</div>
    <div class="stat-value" style="font-size: var(--trusty-fs-lg)">
      {#if health?.status === 'ok'}
        <span class="badge badge-success">healthy</span>
      {:else}
        <span class="badge badge-danger">offline</span>
      {/if}
    </div>
    <div class="stat-meta">daemon</div>
  </div>
  <div class="stat">
    <div class="stat-label">Uptime</div>
    <div class="stat-value">{humanUptime(health?.uptime_secs)}</div>
    <div class="stat-meta">since start</div>
  </div>
  <div class="stat">
    <div class="stat-label">Version</div>
    <div class="stat-value text-mono" style="font-size: var(--trusty-fs-lg)">
      {health?.version || '—'}
    </div>
    <div class="stat-meta">trusty-search</div>
  </div>
  <div class="stat">
    <div class="stat-label">Chunks</div>
    <div class="stat-value">{totalChunks.toLocaleString()}</div>
    <div class="stat-meta">
      across {indexes.length} index{indexes.length === 1 ? '' : 'es'}
    </div>
  </div>
</div>

<div class="card mb-4">
  <div class="card-header flex-between">
    <span>Memory limits</span>
    {#if dirty}
      <span class="badge badge-warning">unsaved changes</span>
    {/if}
  </div>
  <div class="card-body">
    {#if configError}
      <p class="text-sm mb-3" style="color: var(--trusty-danger)">{configError}</p>
    {/if}
    {#if saveMessage}
      <p class="text-sm mb-3" style="color: var(--trusty-success)">{saveMessage}</p>
    {/if}
    {#if config == null && !configError}
      <p class="text-muted text-sm">Loading current configuration…</p>
    {:else}
      <div class="form-group">
        <label class="form-label" for="mem-limit">
          Daemon memory limit (MB) — blank disables the cap
        </label>
        <input
          id="mem-limit"
          type="text"
          inputmode="numeric"
          class="input"
          class:dirty={memoryDirty}
          placeholder="unlimited"
          bind:value={memoryLimit}
        />
        {#if memoryDirty}
          <div class="diff">
            current:
            <code>{config.memory_limit_mb == null ? 'unlimited' : config.memory_limit_mb + ' MB'}</code>
            → pending:
            <code>{memoryLimit.trim() === '' ? 'unlimited' : memoryLimit.trim() + ' MB'}</code>
          </div>
        {/if}
      </div>
      <div class="form-group">
        <label class="form-label" for="idx-mem-limit">
          Per-index memory limit (MB) — blank disables the cap
        </label>
        <input
          id="idx-mem-limit"
          type="text"
          inputmode="numeric"
          class="input"
          class:dirty={indexMemoryDirty}
          placeholder="unlimited"
          bind:value={indexMemoryLimit}
        />
        {#if indexMemoryDirty}
          <div class="diff">
            current:
            <code
              >{config.index_memory_limit_mb == null
                ? 'unlimited'
                : config.index_memory_limit_mb + ' MB'}</code
            >
            → pending:
            <code>{indexMemoryLimit.trim() === '' ? 'unlimited' : indexMemoryLimit.trim() + ' MB'}</code>
          </div>
        {/if}
      </div>
      <div class="flex-gap-2">
        <button class="btn btn-primary" disabled={!dirty || saving} onclick={save}>
          {saving ? 'Saving…' : 'Save changes'}
        </button>
        <button class="btn" disabled={!dirty || saving} onclick={reset}>Reset</button>
      </div>
    {/if}
  </div>
</div>

<div class="card">
  <div class="card-header">Daemon details</div>
  <div class="card-body" style="padding: 0">
    <table class="table">
      <tbody>
        <tr>
          <th style="width: 240px">OpenRouter chat</th>
          <td>
            {#if openrouterEnabled}
              <span class="badge badge-success">enabled</span>
              <span class="text-muted text-sm">OPENROUTER_API_KEY detected</span>
            {:else}
              <span class="badge badge-muted">disabled</span>
              <span class="text-muted text-sm">
                Set <code>OPENROUTER_API_KEY</code> and restart the daemon to enable
                <code>/chat</code>.
              </span>
            {/if}
          </td>
        </tr>
        <tr>
          <th>Daemon port</th>
          <td class="text-mono">{daemonPort ?? '—'}</td>
        </tr>
        <tr>
          <th>API base URL</th>
          <td class="text-mono">
            {typeof window !== 'undefined' ? window.location.origin : '—'}
          </td>
        </tr>
        <tr>
          <th>Indexes registered</th>
          <td>{indexes.length}</td>
        </tr>
        <tr>
          <th>Total chunks</th>
          <td>{totalChunks.toLocaleString()}</td>
        </tr>
        <tr>
          <th>Data directory</th>
          <td class="text-muted text-sm">
            Managed by the daemon — see <code>trusty-search doctor</code> for the
            resolved path on this machine.
          </td>
        </tr>
      </tbody>
    </table>
  </div>
</div>

<style>
  .page-title {
    font-size: var(--trusty-fs-xl);
    margin: 0 0 var(--trusty-space-5) 0;
    font-weight: 600;
  }
  .table th {
    background: var(--trusty-content-bg);
    text-transform: none;
    letter-spacing: 0;
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-text-secondary);
  }
  .input.dirty {
    border-color: var(--trusty-warning);
    background: var(--trusty-warning-soft);
  }
  .diff {
    margin-top: var(--trusty-space-2);
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-muted);
  }
  .diff code {
    background: #f1f5f9;
    padding: 1px 5px;
    border-radius: var(--trusty-radius-sm);
  }
</style>
