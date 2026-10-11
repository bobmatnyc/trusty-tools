<script>
  /*
   * Why: #9067, DOC-74 §15.6 "Page (S4)" — scope picker, key list, set,
   * delete and copy, each key with an "agents may use" toggle, default OFF.
   * What: one page over `lib/api.js`. The project comes only from the tm
   * registry picker; scopes are the project and owner scopes only; the key
   * list shows name, length and time, never a value. A set clears its value
   * input before the call and shows only the server's masked text, dropped
   * on the next action. Copy stays in the chosen project and names backends
   * from `backends`, never a fixed list. A refused toggle reverts.
   * Test: `src/App.test.js`.
   */
  import { onMount } from 'svelte';
  import { createApi } from './lib/api.js';
  import { errorLines } from './lib/kinds.js';

  let { api = createApi() } = $props();

  let projects = $state([]);
  let project = $state('');
  let scopes = $state([]);
  let vault = $state('');
  let keys = $state([]);
  let backends = $state([]);
  let selectedBackend = $state('');
  let notice = $state(null);

  let setKey = $state('');
  let setValue = $state('');
  let masked = $state(null);
  let pendingDelete = $state(null);
  let fromBackend = $state('');
  let toBackend = $state('');
  let copied = $state(null);

  const available = $derived(backends.filter((b) => b.available === true));

  function fail(err) {
    notice = { tone: 'error', lines: errorLines(err) };
  }

  function said(text) {
    notice = { tone: 'ok', lines: [text] };
  }

  /** Forget every one-shot result before a new action. */
  function clearResults() {
    notice = null;
    masked = null;
    copied = null;
    pendingDelete = null;
  }

  onMount(async () => {
    try {
      projects = (await api.projects()).projects ?? [];
    } catch (err) {
      fail(err);
    }
  });

  async function chooseProject(path) {
    clearResults();
    project = path;
    scopes = [];
    vault = '';
    keys = [];
    backends = [];
    if (!path) return;
    try {
      const answer = await api.scopes(path);
      // #9067: no machine scope, whatever an answer carried.
      scopes = (answer.scopes ?? []).filter((s) => s.kind === 'project' || s.kind === 'owner');
    } catch (err) {
      fail(err);
      return;
    }
    if (scopes.length > 0) await chooseScope(scopes[0].vault);
    try {
      const answer = await api.backends(path);
      backends = answer.backends ?? [];
      selectedBackend = answer.selected ?? '';
      fromBackend = available[0]?.id ?? '';
      toBackend = available.find((b) => b.id !== fromBackend)?.id ?? '';
    } catch (err) {
      fail(err);
    }
  }

  async function chooseScope(next) {
    vault = next;
    await refresh();
  }

  async function refresh() {
    if (!project || !vault) return;
    try {
      keys = (await api.list(project, vault)).keys ?? [];
    } catch (err) {
      keys = [];
      fail(err);
    }
  }

  async function submitSet(event) {
    event.preventDefault();
    clearResults();
    const key = setKey.trim();
    const value = setValue;
    // #9067: the input is cleared before the call; the page keeps no copy.
    setValue = '';
    if (key === '' || value === '') {
      notice = { tone: 'error', lines: ['Enter a key name and a value.'] };
      return;
    }
    try {
      const answer = await api.set(project, vault, key, value);
      masked = { key, outcome: answer.outcome, text: answer.masked };
      setKey = '';
      await refresh();
    } catch (err) {
      fail(err);
    }
  }

  async function confirmDelete(name) {
    clearResults();
    try {
      const answer = await api.remove(project, vault, name);
      said(answer.removed ? `Deleted ${name}.` : `${name} was not there.`);
      await refresh();
    } catch (err) {
      fail(err);
    }
  }

  async function toggle(row, allowed) {
    clearResults();
    const before = row.agents_may_use === true;
    row.agents_may_use = allowed;
    try {
      const answer = await api.agentsMayUse(project, vault, row.name, allowed);
      row.agents_may_use = answer.allowed === true;
    } catch (err) {
      // #9067: a refusal (agent_use_refused included) puts the toggle back.
      row.agents_may_use = before;
      fail(err);
    }
  }

  async function submitCopy(event) {
    event.preventDefault();
    clearResults();
    try {
      copied = await api.copy(project, fromBackend, toBackend);
      await refresh();
    } catch (err) {
      fail(err);
    }
  }
</script>

<main>
  <header>
    <h1>Trusty Secrets</h1>
    <p class="muted">Values are never shown here. Set one, and only its masked form comes back.</p>
  </header>

  <section aria-label="Project">
    <label>
      Project
      <select
        data-testid="project"
        value={project}
        onchange={(e) => chooseProject(e.currentTarget.value)}
      >
        <option value="">Choose a registered project</option>
        {#each projects as p (p.path)}
          <option value={p.path}>{p.name} — {p.path}</option>
        {/each}
      </select>
    </label>
    {#if projects.length === 0}
      <p class="muted">No registered projects. Register one with tm first.</p>
    {/if}
  </section>

  {#if notice}
    <div class={notice.tone === 'error' ? 'notice error' : 'notice'} role="status" data-testid="notice">
      {#each notice.lines as line}
        <p>{line}</p>
      {/each}
    </div>
  {/if}

  {#if project && scopes.length > 0}
    <section aria-label="Scope">
      <label>
        Scope
        <select data-testid="scope" value={vault} onchange={(e) => chooseScope(e.currentTarget.value)}>
          {#each scopes as s (s.vault)}
            <option value={s.vault}>{s.kind}: {s.vault}</option>
          {/each}
        </select>
      </label>
    </section>

    <section aria-label="Keys">
      <h2>Keys</h2>
      {#if keys.length === 0}
        <p class="muted">No keys in this scope.</p>
      {:else}
        <table data-testid="keys">
          <thead>
            <tr><th>Name</th><th>Length</th><th>Updated</th><th>Agents may use</th><th></th></tr>
          </thead>
          <tbody>
            {#each keys as row (row.name)}
              <tr data-key={row.name}>
                <td class="mono">{row.name}</td>
                <td>{row.length}</td>
                <td>{row.updated_at}</td>
                <td>
                  <input
                    type="checkbox"
                    aria-label={`Agents may use ${row.name}`}
                    data-testid="agents-toggle"
                    checked={row.agents_may_use === true}
                    onchange={(e) => toggle(row, e.currentTarget.checked)}
                  />
                </td>
                <td>
                  {#if pendingDelete === row.name}
                    <button data-testid="confirm-delete" onclick={() => confirmDelete(row.name)}>Confirm delete</button>
                    <button onclick={() => (pendingDelete = null)}>Cancel</button>
                  {:else}
                    <button data-testid="delete" onclick={() => (pendingDelete = row.name)}>Delete</button>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>

    <section aria-label="Set a value">
      <h2>Set a value</h2>
      <form data-testid="set-form" onsubmit={submitSet} autocomplete="off">
        <label>Key <input data-testid="set-key" bind:value={setKey} maxlength="256" /></label>
        <label>
          Value
          <input
            data-testid="set-value"
            type="password"
            autocomplete="new-password"
            spellcheck="false"
            bind:value={setValue}
          />
        </label>
        <button type="submit">Set</button>
      </form>
      {#if masked}
        <p data-testid="masked">{masked.key} {masked.outcome}: <span class="mono">{masked.text}</span></p>
      {/if}
    </section>

    <section aria-label="Copy">
      <h2>Copy between backends</h2>
      <p class="muted">Copies every key of this project from one backend to another. Writes use {selectedBackend || 'the selected backend'}.</p>
      {#if available.length < 2}
        <p class="muted">Copy needs two available backends on this machine.</p>
      {:else}
        <form data-testid="copy-form" onsubmit={submitCopy}>
          <label>
            From
            <select data-testid="copy-from" bind:value={fromBackend}>
              {#each available as b (b.id)}<option value={b.id}>{b.id}</option>{/each}
            </select>
          </label>
          <label>
            To
            <select data-testid="copy-to" bind:value={toBackend}>
              {#each available as b (b.id)}<option value={b.id}>{b.id}</option>{/each}
            </select>
          </label>
          <button type="submit">Copy</button>
        </form>
      {/if}
      {#if copied}
        <p data-testid="copied">Copied {copied.copied?.length ?? 0}; failed {copied.failed?.length ?? 0}.</p>
      {/if}
    </section>
  {/if}
</main>

<style>
  main {
    max-width: 960px;
    margin: 0 auto;
    padding: var(--trusty-space-5);
    font-family: var(--trusty-font);
    color: var(--text);
  }
  :global(body) {
    margin: 0;
    background: var(--bg);
  }
  section {
    margin-top: var(--trusty-space-5);
  }
  label {
    display: inline-flex;
    gap: var(--trusty-space-2);
    align-items: center;
    margin-right: var(--trusty-space-4);
  }
  table {
    width: 100%;
    border-collapse: collapse;
  }
  th,
  td {
    text-align: left;
    padding: var(--trusty-space-2);
    border-bottom: 1px solid var(--border);
  }
  .mono {
    font-family: var(--trusty-mono);
  }
  .muted {
    color: var(--muted);
  }
  .notice {
    margin-top: var(--trusty-space-4);
    padding: var(--trusty-space-3);
    border: 1px solid var(--border);
    border-radius: var(--trusty-radius);
  }
  .notice.error {
    border-color: var(--red);
  }
  .notice p {
    margin: 0;
  }
</style>
