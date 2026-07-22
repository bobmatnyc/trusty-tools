<script>
  import { onMount } from 'svelte';
  import RefreshHeader from './RefreshHeader.svelte';

  /**
   * Why: The Config tab lets operators edit trusty-mpm's
   *      `~/.trusty-tools/trusty-mpm/config.yaml` (the #1220 cross-crate config
   *      convention) from the console — the single HTTP front door (#1104) — so
   *      they never hand-edit YAML or set env vars to change the managed-session
   *      workspace root, the auto-resume default, or the default model.
   * What: loads GET /api/console/config/mpm into a form, lets the operator edit
   *      the three fields, and saves via POST /api/console/config/mpm (which routes
   *      to the trusty-mpm `config_write` MCP tool). Shows the resolved absolute
   *      `workspace_root` so the effective path is visible even when the template
   *      field is blank.
   * Test: with no daemon the routes return 503 and the tab shows the
   *      "not available" state without erroring; a successful save round-trips the
   *      resolved root back into the form.
   */

  let config = $state(null);       // { workspace_root_template, auto_resume, default_model, workspace_root }
  let loading = $state(true);
  let error = $state(null);
  let refreshing = $state(false);
  let saving = $state(false);
  let saveMsg = $state(null);

  // Editable form fields (decoupled from the loaded config so edits are local
  // until the operator saves).
  let template = $state('');
  let autoResume = $state(false);
  let defaultModel = $state('');

  async function load() {
    refreshing = true;
    error = null;
    saveMsg = null;
    try {
      const resp = await fetch('/api/console/config/mpm');
      if (resp.status === 503) {
        error = 'trusty-mpm is not available (daemon not running or binary not on PATH).';
        config = null;
        return;
      }
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      config = await resp.json();
      template = config.workspace_root_template ?? '';
      autoResume = config.auto_resume ?? false;
      defaultModel = config.default_model ?? '';
    } catch (e) {
      error = e.message;
      config = null;
    } finally {
      loading = false;
      refreshing = false;
    }
  }

  async function save() {
    saving = true;
    saveMsg = null;
    error = null;
    try {
      // Send only fields the operator filled in; a blank template/model is sent
      // as null-equivalent by omitting it so an empty box does not clobber an
      // intentional value. (Operators clear a field by leaving it blank.)
      const body = {
        workspace_root_template: template.trim() === '' ? null : template.trim(),
        auto_resume: autoResume,
        default_model: defaultModel.trim() === '' ? null : defaultModel.trim(),
      };
      const resp = await fetch('/api/console/config/mpm', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (resp.status === 503) {
        error = 'trusty-mpm is not available; cannot save.';
        return;
      }
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      config = await resp.json();
      template = config.workspace_root_template ?? '';
      autoResume = config.auto_resume ?? false;
      defaultModel = config.default_model ?? '';
      saveMsg = 'Saved to ~/.trusty-tools/trusty-mpm/config.yaml';
    } catch (e) {
      error = e.message;
    } finally {
      saving = false;
    }
  }

  onMount(load);
</script>

<RefreshHeader title="trusty-mpm Configuration" onRefresh={load} {refreshing} />

{#if loading}
  <div class="msg">Loading configuration…</div>
{:else if error}
  <div class="msg err">{error}</div>
{/if}

{#if config}
  <form class="cfg-form" onsubmit={(e) => { e.preventDefault(); save(); }}>
    <label class="field">
      <span class="label">Workspace root template</span>
      <input
        type="text"
        bind:value={template}
        placeholder="~/trusty-mpm-projects"
        autocomplete="off"
      />
      <span class="hint">
        New sessions are provisioned under
        <code>&lt;root&gt;/&lt;owner&gt;/&lt;repo&gt;/&lt;session-id&gt;</code>.
        Leave blank for the default <code>~/trusty-mpm-projects</code>.
        Effective root: <code>{config.workspace_root}</code>
      </span>
    </label>

    <label class="field checkbox">
      <input type="checkbox" bind:checked={autoResume} />
      <span class="label">Supervisor auto-resume default</span>
    </label>

    <label class="field">
      <span class="label">Default model</span>
      <input
        type="text"
        bind:value={defaultModel}
        placeholder="(unset — uses ~/.trusty-mpm/config.toml)"
        autocomplete="off"
      />
      <span class="hint">Model id or tier alias (<code>haiku</code>/<code>sonnet</code>/<code>opus</code>) for launched sessions.</span>
    </label>

    <div class="actions">
      <button type="submit" class="save-btn" disabled={saving}>
        {saving ? 'Saving…' : 'Save'}
      </button>
      {#if saveMsg}<span class="save-msg">{saveMsg}</span>{/if}
    </div>
  </form>
{/if}

<style>
  .cfg-form {
    display: flex;
    flex-direction: column;
    gap: 1.25rem;
    max-width: 640px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }
  .field.checkbox {
    flex-direction: row;
    align-items: center;
    gap: 0.5rem;
  }
  .label {
    font-size: 0.85rem;
    font-weight: 600;
    color: var(--trusty-text-primary);
  }
  input[type='text'] {
    padding: 0.5rem 0.65rem;
    border: 1px solid var(--trusty-border);
    border-radius: 0.4rem;
    background: var(--trusty-card-bg);
    color: var(--trusty-text-primary);
    font-size: 0.85rem;
  }
  input[type='text']:focus {
    outline: none;
    border-color: var(--trusty-accent);
  }
  .hint {
    font-size: 0.75rem;
    color: var(--trusty-text-secondary);
    line-height: 1.4;
  }
  code {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.72rem;
    background: color-mix(in srgb, var(--trusty-accent) 8%, transparent);
    padding: 0.05rem 0.3rem;
    border-radius: 0.25rem;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }
  .save-btn {
    background: var(--trusty-accent);
    border: none;
    border-radius: 0.4rem;
    color: white;
    cursor: pointer;
    font-size: 0.85rem;
    font-weight: 600;
    padding: 0.45rem 1.1rem;
  }
  .save-btn:disabled { opacity: 0.5; cursor: default; }
  .save-msg { font-size: 0.78rem; color: var(--trusty-success, #16a34a); }
  .msg {
    padding: 1.25rem;
    border-radius: 0.5rem;
    background: var(--trusty-card-bg);
    color: var(--trusty-text-secondary);
    margin-bottom: 1rem;
  }
  .msg.err { color: var(--trusty-danger); }
</style>
