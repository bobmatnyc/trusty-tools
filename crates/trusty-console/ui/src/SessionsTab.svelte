<script>
  import { onMount, onDestroy } from 'svelte';
  import RefreshHeader from './RefreshHeader.svelte';
  import { autoResumeEffective, autoResumeLabel } from './autoResume.js';
  import {
    GROUP_ORDER,
    OTHER_STATE,
    failedRows,
    groupByState,
    isUnknown,
    lastUsedLabel,
    lastUsedTitleFor,
    nameOf,
    rawState,
    reportedStatus,
    sortByLastUsed,
    summariseBulkDelete,
  } from './sessionRows.js';

  /**
   * Why: The Sessions tab renders the trusty-mpm managed-session fleet natively
   *      from the console's MCP-backed session routes (#1222 P2) — never by
   *      proxying to the daemon HTTP (#1104). Operators watch lifecycle state,
   *      drive stop/resume/decommission/spawn, view the live pane, and toggle
   *      supervisor auto-resume (RFC §6 Q6), all through the single HTTP front
   *      door (P3).
   * What: polls GET /api/console/sessions (fleet) + /supervisor (fleet counts +
   *      auto-resume) at a configurable interval (default 5 s — the RFC's 15 s
   *      default was flagged too coarse for actively-failing/auto-resuming
   *      sessions); issues control POSTs/DELETE; renders an activity pane on
   *      demand.
   * Test: with no daemon, the routes return 503 and the tab shows the
   *      "not available" state without erroring. The bucketing, last-used
   *      comparator, and bulk-delete reporting are unit-tested in
   *      `sessionRows.test.js`.
   *
   * #6430: every row shows its last activity (or "never"), and one control
   *      orders every group by it, with no-activity rows always last.
   * #6431: the unknown bucket — records whose `state` is missing or
   *      unrecognised, which in practice are the daemon's legacy registry
   *      entries — gets multi-select and a record-only bulk delete. `deleted`
   *      tombstones now have their own group, so they are no longer swept into
   *      that bucket. Deletion never removes a worktree or workspace (#1511).
   */

  // ── poll interval (RFC Q3: poll-based refresh; 15s flagged too coarse) ──────
  // Default 5s for watching active sessions; configurable in the UI.
  const POLL_OPTIONS = [3, 5, 10, 30];
  let pollSecs = $state(5);

  let sessions = $state([]);
  let supervisor = $state(null);
  let loading = $state(true);
  let error = $state(null);
  let refreshing = $state(false);
  let busyId = $state(null); // id of a session whose control op is in flight
  let actionMsg = $state(null);

  // Activity pane state — keyed by session id so concurrent fetches for
  // different sessions never clobber each other (review finding #1). Each entry
  // is `{ lines, loading }`. A SvelteMap keeps reads reactive in Svelte 5 runes.
  let activeActivityId = $state(null);
  let activityById = $state(new Map()); // id -> { lines: string, loading: bool }

  function activityFor(id) {
    return activityById.get(id) ?? { lines: '', loading: false };
  }
  function setActivity(id, patch) {
    const next = new Map(activityById);
    next.set(id, { ...activityFor(id), ...patch });
    activityById = next;
  }

  // Spawn form state.
  let showSpawn = $state(false);
  let spawnRepo = $state('');
  let spawnRef = $state('main');
  let spawnTask = $state('');
  let spawnBusy = $state(false);

  // #6430/#6431: bucketing, the last-used comparator, and the unknown-set
  // predicate live in ./sessionRows.js so they are testable without a browser —
  // the bulk action's target set in particular must be an asserted predicate,
  // not a `||` fallback in a template.
  let sortDir = $state('desc'); // #6430: last-used order, newest first.

  let grouped = $derived.by(() => {
    const g = groupByState(sessions);
    for (const key of GROUP_ORDER) g[key] = sortByLastUsed(g[key], sortDir);
    return g;
  });

  // #6431: bulk delete of the unknown bucket. Nothing is auto-selected, and the
  // selection can only ever contain unknown-bucket ids.
  let selected = $state(new Set());
  let confirming = $state(false);
  let bulkBusy = $state(false);
  let bulkFailures = $state([]);

  let unknownRows = $derived(grouped[OTHER_STATE] ?? []);
  let selectedRows = $derived(unknownRows.filter((s) => selected.has(s.id)));

  function toggleSelected(sess) {
    // Guard the invariant in code, not only in the template: only an
    // unknown-bucket row is ever selectable.
    if (!isUnknown(sess)) return;
    const next = new Set(selected);
    if (next.has(sess.id)) next.delete(sess.id);
    else next.add(sess.id);
    selected = next;
    confirming = false;
  }

  function clearSelection() {
    selected = new Set();
    confirming = false;
  }

  async function runBulkDelete() {
    const ids = selectedRows.map((s) => s.id);
    if (ids.length === 0) return;
    bulkBusy = true;
    bulkFailures = [];
    try {
      const resp = await fetch('/api/console/sessions/bulk-delete', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ session_ids: ids }),
      });
      if (!resp.ok) {
        actionMsg = `bulk delete failed: HTTP ${resp.status}`;
        return;
      }
      const payload = await resp.json();
      actionMsg = summariseBulkDelete(payload);
      bulkFailures = failedRows(payload);
      clearSelection();
      await fetchAll(true);
    } catch (e) {
      actionMsg = `bulk delete error: ${e.message}`;
    } finally {
      bulkBusy = false;
      confirming = false;
    }
  }

  /** Normalise the session_list result into an array of records. */
  function asArray(payload) {
    if (Array.isArray(payload)) return payload;
    if (payload && Array.isArray(payload.sessions)) return payload.sessions;
    return [];
  }

  async function fetchAll(isRefresh = false) {
    if (refreshing || (isRefresh && loading)) return;
    if (isRefresh) refreshing = true; else loading = true;
    try {
      const [listResp, supResp] = await Promise.all([
        fetch('/api/console/sessions'),
        fetch('/api/console/sessions/supervisor'),
      ]);
      if (listResp.status === 503 || supResp.status === 503) {
        error = 'trusty-mpm not available (daemon absent or first boot).';
        sessions = [];
        supervisor = null;
        return;
      }
      if (!listResp.ok) throw new Error(`sessions HTTP ${listResp.status}`);
      sessions = asArray(await listResp.json());
      supervisor = supResp.ok ? await supResp.json() : null;
      error = null;
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
      refreshing = false;
    }
  }

  /** POST/DELETE a control op for a session, then refresh. */
  async function control(id, op) {
    busyId = id;
    actionMsg = null;
    try {
      const method = op === 'decommission' ? 'DELETE' : 'POST';
      const path = op === 'decommission'
        ? `/api/console/sessions/${id}`
        : `/api/console/sessions/${id}/${op}`;
      const resp = await fetch(path, { method });
      if (!resp.ok) {
        actionMsg = `${op} failed: HTTP ${resp.status}`;
        return;
      }
      actionMsg = `${op} ok`;
      await fetchAll(true);
    } catch (e) {
      actionMsg = `${op} error: ${e.message}`;
    } finally {
      busyId = null;
    }
  }

  async function viewActivity(id) {
    if (activeActivityId === id) { activeActivityId = null; return; }
    activeActivityId = id;
    setActivity(id, { loading: true, lines: '' });
    try {
      const resp = await fetch(`/api/console/sessions/${id}/activity?lines=50`);
      // Resolve-time guard: if the operator switched to a different session
      // while this fetch was in flight, drop the stale response so a late reply
      // for session A can never overwrite the pane now showing session B
      // (review finding #1 — the UI race).
      if (activeActivityId !== id) return;
      if (!resp.ok) {
        setActivity(id, { lines: `(unavailable: HTTP ${resp.status})` });
        return;
      }
      const data = await resp.json();
      if (activeActivityId !== id) return; // re-check after the awaited json()
      setActivity(id, { lines: data.raw_pane || '(empty pane)' });
    } catch (e) {
      if (activeActivityId === id) setActivity(id, { lines: `(error: ${e.message})` });
    } finally {
      if (activeActivityId === id) setActivity(id, { loading: false });
    }
  }

  async function spawn() {
    if (!spawnRepo || !spawnTask) { actionMsg = 'repo_url and task are required'; return; }
    spawnBusy = true;
    actionMsg = null;
    try {
      const resp = await fetch('/api/console/sessions', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ repo_url: spawnRepo, ref: spawnRef || 'main', task: spawnTask }),
      });
      if (!resp.ok) { actionMsg = `spawn failed: HTTP ${resp.status}`; return; }
      actionMsg = 'session spawned';
      showSpawn = false;
      spawnRepo = ''; spawnTask = '';
      await fetchAll(true);
    } catch (e) {
      actionMsg = `spawn error: ${e.message}`;
    } finally {
      spawnBusy = false;
    }
  }

  async function toggleAutoResume() {
    // #5208: flip what is actually in force, not what the file happens to say.
    // With no override file and an env-enabled supervisor, `!desired` would send
    // `enabled: true` for a button that reads "Disable".
    const next = !effectiveAutoResume();
    try {
      const resp = await fetch('/api/console/sessions/supervisor/auto-resume', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ enabled: next }),
      });
      if (!resp.ok) { actionMsg = `auto-resume toggle failed: HTTP ${resp.status}`; return; }
      await fetchAll(true);
    } catch (e) {
      actionMsg = `auto-resume error: ${e.message}`;
    }
  }

  let timer;
  function restartTimer() {
    clearInterval(timer);
    timer = setInterval(() => fetchAll(true), pollSecs * 1000);
  }
  $effect(() => { pollSecs; restartTimer(); });

  onMount(async () => { await fetchAll(); restartTimer(); });
  onDestroy(() => clearInterval(timer));

  // #5208: the label and the button both read `effective` (see ./autoResume.js),
  // not `desired` — the latter is only the toggle's saved value and reads "off"
  // while an env-enabled supervisor is actively resuming.
  const effectiveAutoResume = () => autoResumeEffective(supervisor);
</script>

<div class="tab-content">
  <RefreshHeader title="MPM Sessions" onRefresh={() => fetchAll(true)} {refreshing} />

  <!-- Controls row: poll interval + spawn -->
  <div class="controls">
    <label class="poll-ctl">
      Refresh every
      <select bind:value={pollSecs}>
        {#each POLL_OPTIONS as o}<option value={o}>{o}s</option>{/each}
      </select>
    </label>
    <button class="spawn-btn" onclick={() => showSpawn = !showSpawn}>
      {showSpawn ? 'Cancel' : '+ Spawn session'}
    </button>
  </div>

  {#if showSpawn}
    <div class="spawn-form">
      <input placeholder="repo_url (https://github.com/owner/repo)" bind:value={spawnRepo} />
      <input placeholder="ref (default main)" bind:value={spawnRef} />
      <input placeholder="task" bind:value={spawnTask} />
      <button onclick={spawn} disabled={spawnBusy}>{spawnBusy ? 'Spawning…' : 'Spawn'}</button>
    </div>
  {/if}

  {#if actionMsg}<div class="action-msg">{actionMsg}</div>{/if}

  <!-- #6431: fail-closed reporting — every session that was NOT deleted is
       named here, with the daemon's own reason. -->
  {#if bulkFailures.length > 0}
    <ul class="bulk-failures">
      {#each bulkFailures as row (row.session_id)}
        <li><code>{row.session_id}</code> — {row.error ?? 'not deleted'}</li>
      {/each}
    </ul>
  {/if}

  <!-- Supervisor widget -->
  {#if supervisor}
    <div class="supervisor">
      <div class="sup-counts">
        <span class="count active">{supervisor.fleet?.active ?? 0} active</span>
        <span class="count stopped">{supervisor.fleet?.stopped ?? 0} stopped</span>
        <span class="count errored">{supervisor.fleet?.errored ?? 0} errored</span>
        <span class="count total">{supervisor.fleet?.total ?? 0} total</span>
      </div>
      <div class="auto-resume">
        <span
          class="ar-label"
          title="What the supervisor's next sweep will do. With no saved setting this is inferred from the daemon's own environment, which the supervisor process may not share."
        >Auto-resume: <strong>{autoResumeLabel(supervisor)}</strong></span>
        <button
          class="ar-toggle"
          onclick={toggleAutoResume}
          disabled={!!supervisor.auto_resume?.read_error}
        >
          {effectiveAutoResume() ? 'Disable' : 'Enable'}
        </button>
      </div>
    </div>
  {/if}

  {#if loading}
    <div class="placeholder">Loading sessions…</div>
  {:else if error}
    <div class="not-available">{error}</div>
  {:else}
    <!-- #6430: one sort control, applied to every group. -->
    <div class="sort-ctl">
      <label>
        Sort by last used
        <select bind:value={sortDir}>
          <option value="desc">newest first</option>
          <option value="asc">oldest first</option>
        </select>
      </label>
      <span class="sort-note">sessions with no recorded activity sort last</span>
    </div>
    {#each GROUP_ORDER as st}
      {#if grouped[st] && grouped[st].length > 0}
        <h3 class="group-title {st}">{st} ({grouped[st].length})</h3>
        {#if st === OTHER_STATE}
          <!-- #6431: bulk delete, scoped to this bucket only. Nothing is
               pre-selected, and the action is RECORD-deletion only — it never
               removes a worktree or workspace directory (#1511). -->
          <div class="bulk-bar">
            <span class="bulk-count">{selected.size} selected</span>
            <button
              onclick={() => (confirming = true)}
              disabled={selected.size === 0 || bulkBusy}
            >Delete records…</button>
            <button onclick={clearSelection} disabled={selected.size === 0 || bulkBusy}>
              Clear
            </button>
          </div>
          {#if confirming}
            <div class="bulk-confirm">
              <p>
                Delete the RECORDS for these {selectedRows.length} session(s)? Their
                workspace directories and worktrees are left untouched.
              </p>
              <ul>
                {#each selectedRows as sess (sess.id)}
                  <li>
                    {nameOf(sess)} — <code>{sess.id}</code>
                    <!-- #6431: show the reported status too. An unknown-bucket
                         row labels itself `unknown`, so without this the dialog
                         hides the one liveness hint it actually has. -->
                    {#if reportedStatus(sess)}<span class="bulk-status">({reportedStatus(sess)})</span>{/if}
                  </li>
                {/each}
              </ul>
              <p class="bulk-note">
                A session that is still running is refused, not deleted.
              </p>
              <button class="danger" onclick={runBulkDelete} disabled={bulkBusy}>
                {bulkBusy ? 'Deleting…' : `Delete ${selectedRows.length} record(s)`}
              </button>
              <button onclick={() => (confirming = false)} disabled={bulkBusy}>Cancel</button>
            </div>
          {/if}
        {/if}
        <div class="session-list">
          {#each grouped[st] as sess (sess.id)}
            {@const state = rawState(sess)}
            {@const act = activityFor(sess.id)}
            <div class="session-card">
              <div class="session-head">
                {#if st === OTHER_STATE}
                  <input
                    type="checkbox"
                    checked={selected.has(sess.id)}
                    onchange={() => toggleSelected(sess)}
                    aria-label={`Select ${nameOf(sess)} for record deletion`}
                  />
                {/if}
                <span class="session-name">{nameOf(sess)}</span>
                <!-- Show the session's actual reported state. For known states
                     this equals the group; for the catch-all it surfaces the
                     raw lifecycle label so unexpected states stay visible. -->
                <span class="session-state {st}">{state}</span>
              </div>
              <div class="session-id">{sess.id}</div>
              <!-- #6430: last activity, worded exactly as the Search and Memory
                   tabs word it (#6424's lastUsed.js). -->
              <div class="session-last-used" title={lastUsedTitleFor(sess)}>
                Last used: {lastUsedLabel(sess)}
              </div>
              <div class="session-actions">
                <button onclick={() => viewActivity(sess.id)} disabled={busyId === sess.id}>
                  {activeActivityId === sess.id ? 'Hide' : 'Activity'}
                </button>
                {#if state === 'stopped' || state === 'errored'}
                  <button onclick={() => control(sess.id, 'resume')} disabled={busyId === sess.id}>Resume</button>
                {/if}
                {#if state === 'active' || state === 'provisioning'}
                  <button onclick={() => control(sess.id, 'stop')} disabled={busyId === sess.id}>Stop</button>
                {/if}
                {#if state !== 'decommissioned' && state !== 'deleted'}
                  <button class="danger" onclick={() => control(sess.id, 'decommission')} disabled={busyId === sess.id}>Decommission</button>
                {/if}
              </div>
              {#if activeActivityId === sess.id}
                <pre class="activity">{act.loading ? 'Loading pane…' : act.lines}</pre>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    {/each}
    {#if sessions.length === 0}
      <div class="placeholder">No managed sessions. Spawn one to get started.</div>
    {/if}
  {/if}
</div>

<style>
  .tab-content { padding: 0.25rem 0; }
  .placeholder, .not-available {
    background: var(--trusty-card-bg); border-radius: 0.5rem;
    padding: 1.25rem; color: var(--trusty-text-secondary); font-size: 0.9rem;
  }
  .not-available { color: var(--trusty-warning); }

  .controls { display: flex; align-items: center; gap: 1rem; margin-bottom: 1rem; flex-wrap: wrap; }
  .poll-ctl { font-size: 0.8rem; color: var(--trusty-text-secondary); display: flex; align-items: center; gap: 0.4rem; }
  .poll-ctl select { background: var(--trusty-card-bg); color: var(--trusty-text-primary); border: 1px solid var(--trusty-border); border-radius: 0.3rem; padding: 0.2rem 0.4rem; }
  .spawn-btn, .ar-toggle {
    background: none; border: 1px solid var(--trusty-border-strong); border-radius: 0.4rem;
    color: var(--trusty-accent); cursor: pointer; font-size: 0.78rem; padding: 0.3rem 0.7rem;
  }
  .spawn-form { display: flex; gap: 0.5rem; flex-wrap: wrap; margin-bottom: 1rem; }
  .spawn-form input {
    flex: 1; min-width: 160px; background: var(--trusty-card-bg); color: var(--trusty-text-primary);
    border: 1px solid var(--trusty-border); border-radius: 0.3rem; padding: 0.35rem 0.5rem; font-size: 0.85rem;
  }
  .spawn-form button { background: var(--trusty-accent); color: #fff; border: none; border-radius: 0.3rem; padding: 0.35rem 0.8rem; cursor: pointer; }
  .action-msg { font-size: 0.8rem; color: var(--trusty-text-secondary); margin-bottom: 0.75rem; }

  .supervisor {
    display: flex; justify-content: space-between; align-items: center; gap: 1rem; flex-wrap: wrap;
    background: var(--trusty-card-bg); border: 1px solid var(--trusty-border); border-radius: 0.5rem;
    padding: 0.75rem 1rem; margin-bottom: 1.25rem;
  }
  .sup-counts { display: flex; gap: 0.75rem; flex-wrap: wrap; }
  .count { font-size: 0.8rem; padding: 0.15rem 0.5rem; border-radius: 9999px; border: 1px solid var(--trusty-border); }
  .count.active { color: var(--trusty-success); }
  .count.errored { color: var(--trusty-danger); }
  .auto-resume { display: flex; align-items: center; gap: 0.6rem; }
  .ar-label { font-size: 0.8rem; color: var(--trusty-text-secondary); }

  .group-title { font-size: 0.85rem; font-weight: 600; text-transform: uppercase; letter-spacing: 0.05em; margin: 1rem 0 0.5rem; color: var(--trusty-text-secondary); }
  .group-title.active { color: var(--trusty-success); }
  .group-title.errored { color: var(--trusty-danger); }
  .group-title.deleted { color: var(--trusty-text-muted); }
  .group-title.other { color: var(--trusty-warning); }

  .session-list { display: flex; flex-direction: column; gap: 0.5rem; }
  .session-card { background: var(--trusty-card-bg); border: 1px solid var(--trusty-border); border-radius: 0.5rem; padding: 0.75rem 1rem; }
  .session-head { display: flex; justify-content: space-between; align-items: center; gap: 0.5rem; }
  .session-name { font-weight: 600; color: var(--trusty-text-primary); font-size: 0.9rem; word-break: break-word; }
  .session-state { font-size: 0.7rem; padding: 0.1rem 0.45rem; border-radius: 9999px; border: 1px solid var(--trusty-border); color: var(--trusty-text-secondary); }
  .session-state.active { color: var(--trusty-success); }
  .session-state.errored { color: var(--trusty-danger); }
  .session-state.deleted { color: var(--trusty-text-muted); }
  .session-state.other { color: var(--trusty-warning); }
  .session-id { font-family: 'JetBrains Mono', monospace; font-size: 0.72rem; color: var(--trusty-text-muted); margin: 0.2rem 0 0.15rem; }
  .session-last-used { font-size: 0.72rem; color: var(--trusty-text-secondary); margin-bottom: 0.5rem; }

  .sort-ctl { display: flex; align-items: center; gap: 0.75rem; flex-wrap: wrap; font-size: 0.8rem; color: var(--trusty-text-secondary); margin-bottom: 0.5rem; }
  .sort-ctl select { background: var(--trusty-card-bg); color: var(--trusty-text-primary); border: 1px solid var(--trusty-border); border-radius: 0.3rem; padding: 0.2rem 0.4rem; margin-left: 0.4rem; }
  .sort-note { color: var(--trusty-text-muted); }

  .bulk-bar { display: flex; align-items: center; gap: 0.5rem; flex-wrap: wrap; margin-bottom: 0.5rem; }
  .bulk-count { font-size: 0.78rem; color: var(--trusty-text-secondary); }
  .bulk-bar button, .bulk-confirm button {
    background: none; border: 1px solid var(--trusty-border-strong); border-radius: 0.3rem;
    color: var(--trusty-accent); cursor: pointer; font-size: 0.72rem; padding: 0.2rem 0.55rem;
  }
  .bulk-bar button:disabled, .bulk-confirm button:disabled { opacity: 0.5; cursor: default; }
  .bulk-confirm {
    background: var(--trusty-card-bg); border: 1px solid var(--trusty-warning);
    border-radius: 0.5rem; padding: 0.75rem 1rem; margin-bottom: 0.75rem; font-size: 0.8rem;
    color: var(--trusty-text-primary);
  }
  .bulk-confirm ul { margin: 0.5rem 0; padding-left: 1.1rem; }
  .bulk-status { color: var(--trusty-text-secondary); }
  .bulk-note { color: var(--trusty-text-secondary); margin: 0.25rem 0 0.6rem; }
  .bulk-confirm button.danger { color: var(--trusty-danger); }
  .bulk-failures {
    font-size: 0.78rem; color: var(--trusty-danger); margin: 0 0 0.75rem; padding-left: 1.1rem;
  }
  .session-actions { display: flex; gap: 0.4rem; flex-wrap: wrap; }
  .session-actions button {
    background: none; border: 1px solid var(--trusty-border-strong); border-radius: 0.3rem;
    color: var(--trusty-accent); cursor: pointer; font-size: 0.72rem; padding: 0.2rem 0.55rem;
  }
  .session-actions button.danger { color: var(--trusty-danger); }
  .session-actions button:disabled { opacity: 0.5; cursor: default; }
  .activity {
    margin-top: 0.6rem; background: var(--trusty-content-bg); border: 1px solid var(--trusty-border);
    border-radius: 0.4rem; padding: 0.6rem; font-family: 'JetBrains Mono', monospace;
    font-size: 0.72rem; color: var(--trusty-text-secondary); max-height: 240px; overflow: auto; white-space: pre-wrap;
  }
</style>
