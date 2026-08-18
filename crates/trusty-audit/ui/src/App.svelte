<script lang="ts">
  import { onMount } from 'svelte';
  import { guided, type GuidedView, type NextStepView } from './lib/session';

  // Why: three states, and the window must never show a blank panel for any of
  // them. A failed `Command::Guided` shows its reason; a pending one says so.
  let status = $state<GuidedView | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(true);

  async function load() {
    loading = true;
    error = null;
    try {
      status = await guided();
    } catch (e) {
      // Tauri rejects with the `AuditError`'s Display text.
      error = typeof e === 'string' ? e : String(e);
    } finally {
      loading = false;
    }
  }

  onMount(load);

  /**
   * The sentence for a next step.
   *
   * Rendering belongs to the front end (DOC-68 §11), so the wording lives here
   * rather than being computed in Rust and shipped as a string. The CLI phrases
   * the same states as shell commands; this window phrases them as instructions,
   * because its reader has no terminal open.
   */
  function describeNext(next: NextStepView): string {
    switch (next.kind) {
      case 'select-repositories':
        return 'Pick the repositories to audit.';
      case 'install-tools':
        return `Install the pinned tools: ${next.missing.join(', ')}.`;
      case 'ready-for-run':
        return 'Everything is in place — run the audit sweep.';
      case 'return-package':
        return 'Assemble the deliverable and send it back.';
    }
  }

  const installedCount = $derived(status ? status.tools.filter((t) => t.installed).length : 0);
</script>

<header>
  <h1>Trusty Audit</h1>
  <p class="lede">
    Everything this client writes lives under one directory. Deleting that
    directory removes all of it.
  </p>
</header>

{#if loading}
  <p class="muted">Reading the working directory…</p>
{:else if error}
  <section class="panel failed">
    <h2>Could not read the engagement</h2>
    <p class="reason">{error}</p>
    <button onclick={load}>Try again</button>
  </section>
{:else if status}
  <section class="panel">
    <h2>Working directory</h2>
    <p class="path">{status.root}</p>
  </section>

  <section class="panel">
    <h2>Engagement</h2>
    {#if status.manifest}
      <p class="title">{status.manifest.title}</p>
      {#if status.manifest.client}<p class="muted">Client: {status.manifest.client}</p>{/if}
      {#if status.manifest.analyst}<p class="muted">Analyst: {status.manifest.analyst}</p>{/if}
      <p class="muted">
        {status.manifest.repositories.length}
        {status.manifest.repositories.length === 1 ? 'repository' : 'repositories'} configured
      </p>
    {:else}
      <p class="muted">No manifest yet — nothing has run here.</p>
    {/if}
  </section>

  <section class="panel">
    <h2>Tools <span class="muted">{installedCount}/{status.tools.length} installed</span></h2>
    <table>
      <tbody>
        {#each status.tools as tool (tool.name)}
          <tr>
            <td class="mark" class:ok={tool.installed && tool.version !== null}>
              {#if !tool.installed}MISSING{:else if tool.version === null}UNVERIFIED{:else}ok{/if}
            </td>
            <td class="name">{tool.name}</td>
            <td class="version">{tool.version ?? '—'}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </section>

  <section class="panel next">
    <h2>Next step</h2>
    <p>{describeNext(status.next)}</p>
    <p class="muted">
      Phase 1 of the shell shows the engagement's state. Selecting repositories,
      installing the tools, running the sweep and building the return package
      still run from the <code>trusty-audit</code> command line.
    </p>
  </section>
{/if}

<style>
  header {
    margin-bottom: 1.5rem;
  }

  h1 {
    margin: 0;
    font-size: 1.4rem;
    letter-spacing: -0.01em;
  }

  h2 {
    margin: 0 0 0.5rem;
    font-size: 0.8rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--muted);
    font-weight: 600;
  }

  .lede {
    margin: 0.35rem 0 0;
    color: var(--muted);
    font-size: 0.9rem;
  }

  .panel {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 1rem 1.1rem;
    margin-bottom: 1rem;
  }

  .panel.next {
    border-left: 3px solid var(--accent);
  }

  .panel.failed {
    border-left: 3px solid var(--accent);
  }

  .path,
  .reason {
    margin: 0;
    font-family: var(--mono);
    font-size: 0.85rem;
    overflow-wrap: anywhere;
  }

  .title {
    margin: 0;
    font-weight: 600;
  }

  .muted {
    color: var(--muted);
    font-size: 0.88rem;
    margin: 0.35rem 0 0;
  }

  p {
    margin: 0;
  }

  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.88rem;
  }

  td {
    padding: 0.2rem 0;
    vertical-align: baseline;
  }

  .mark {
    width: 6.5rem;
    font-family: var(--mono);
    font-size: 0.75rem;
    color: var(--warn);
  }

  .mark.ok {
    color: var(--ok);
  }

  .name {
    font-family: var(--mono);
  }

  .version {
    text-align: right;
    font-family: var(--mono);
    color: var(--muted);
  }

  code {
    font-family: var(--mono);
    font-size: 0.85em;
  }

  button {
    margin-top: 0.75rem;
    font: inherit;
    font-size: 0.88rem;
    padding: 0.35rem 0.9rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    color: var(--text);
    cursor: pointer;
  }

  button:hover {
    border-color: var(--accent);
  }
</style>
