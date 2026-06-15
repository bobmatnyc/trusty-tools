<script>
  import { onMount, onDestroy } from 'svelte';
  import RefreshHeader from './RefreshHeader.svelte';

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
   *      "not available" state without erroring.
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

  // Lifecycle states we group by, in display order.
  const STATE_ORDER = ['active', 'provisioning', 'stopped', 'errored', 'decommissioned'];
  // Catch-all bucket key for any state the daemon reports that we don't model
  // explicitly — these must still render so unexpected lifecycle states are
  // never silently dropped from the UI (review finding #6).
  const OTHER_STATE = 'other';

  let grouped = $derived.by(() => {
    const g = {};
    for (const s of STATE_ORDER) g[s] = [];
    g[OTHER_STATE] = [];
    for (const sess of sessions) {
      const st = (sess.state || 'unknown').toLowerCase();
      // Known state → its own bucket; anything else → the catch-all so it is
      // still visible (with its raw state label shown on the card).
      if (STATE_ORDER.includes(st)) {
        g[st].push(sess);
      } else {
        g[OTHER_STATE].push(sess);
      }
    }
    return g;
  });

  // Display order for rendering: the known states followed by the catch-all.
  const GROUP_ORDER = [...STATE_ORDER, OTHER_STATE];

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
    const next = !(supervisor?.auto_resume?.desired);
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

  function autoResumeLabel() {
    const ar = supervisor?.auto_resume;
    if (!ar) return '—';
    if (ar.desired && ar.pending_restart) return 'on (restart pending)';
    if (!ar.desired && ar.pending_restart) return 'off (restart pending)';
    return ar.desired ? 'on' : 'off';
  }
</script>

<div class="tab-content">
  <RefreshHeader title="Sessions" onRefresh={() => fetchAll(true)} {refreshing} />

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
        <span class="ar-label">Auto-resume: <strong>{autoResumeLabel()}</strong></span>
        <button class="ar-toggle" onclick={toggleAutoResume}>
          {supervisor.auto_resume?.desired ? 'Disable' : 'Enable'}
        </button>
      </div>
    </div>
  {/if}

  {#if loading}
    <div class="placeholder">Loading sessions…</div>
  {:else if error}
    <div class="not-available">{error}</div>
  {:else}
    {#each GROUP_ORDER as st}
      {#if grouped[st] && grouped[st].length > 0}
        <h3 class="group-title {st}">{st} ({grouped[st].length})</h3>
        <div class="session-list">
          {#each grouped[st] as sess (sess.id)}
            {@const rawState = (sess.state || 'unknown').toLowerCase()}
            {@const act = activityFor(sess.id)}
            <div class="session-card">
              <div class="session-head">
                <span class="session-name">{sess.name || sess.tmux_name || sess.id}</span>
                <!-- Show the session's actual reported state. For known states
                     this equals the group; for the catch-all it surfaces the
                     raw lifecycle label so unexpected states stay visible. -->
                <span class="session-state {st}">{rawState}</span>
              </div>
              <div class="session-id">{sess.id}</div>
              <div class="session-actions">
                <button onclick={() => viewActivity(sess.id)} disabled={busyId === sess.id}>
                  {activeActivityId === sess.id ? 'Hide' : 'Activity'}
                </button>
                {#if rawState === 'stopped' || rawState === 'errored'}
                  <button onclick={() => control(sess.id, 'resume')} disabled={busyId === sess.id}>Resume</button>
                {/if}
                {#if rawState === 'active' || rawState === 'provisioning'}
                  <button onclick={() => control(sess.id, 'stop')} disabled={busyId === sess.id}>Stop</button>
                {/if}
                {#if rawState !== 'decommissioned'}
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
    background: var(--color-surface); border-radius: 0.5rem;
    padding: 1.25rem; color: var(--color-text-secondary); font-size: 0.9rem;
  }
  .not-available { color: var(--color-status-warn); }

  .controls { display: flex; align-items: center; gap: 1rem; margin-bottom: 1rem; flex-wrap: wrap; }
  .poll-ctl { font-size: 0.8rem; color: var(--color-text-secondary); display: flex; align-items: center; gap: 0.4rem; }
  .poll-ctl select { background: var(--color-surface); color: var(--color-text-primary); border: 1px solid var(--color-border); border-radius: 0.3rem; padding: 0.2rem 0.4rem; }
  .spawn-btn, .ar-toggle {
    background: none; border: 1px solid var(--color-border-hover); border-radius: 0.4rem;
    color: var(--color-accent); cursor: pointer; font-size: 0.78rem; padding: 0.3rem 0.7rem;
  }
  .spawn-form { display: flex; gap: 0.5rem; flex-wrap: wrap; margin-bottom: 1rem; }
  .spawn-form input {
    flex: 1; min-width: 160px; background: var(--color-surface); color: var(--color-text-primary);
    border: 1px solid var(--color-border); border-radius: 0.3rem; padding: 0.35rem 0.5rem; font-size: 0.85rem;
  }
  .spawn-form button { background: var(--color-accent); color: #fff; border: none; border-radius: 0.3rem; padding: 0.35rem 0.8rem; cursor: pointer; }
  .action-msg { font-size: 0.8rem; color: var(--color-text-secondary); margin-bottom: 0.75rem; }

  .supervisor {
    display: flex; justify-content: space-between; align-items: center; gap: 1rem; flex-wrap: wrap;
    background: var(--color-surface); border: 1px solid var(--color-border); border-radius: 0.5rem;
    padding: 0.75rem 1rem; margin-bottom: 1.25rem;
  }
  .sup-counts { display: flex; gap: 0.75rem; flex-wrap: wrap; }
  .count { font-size: 0.8rem; padding: 0.15rem 0.5rem; border-radius: 9999px; border: 1px solid var(--color-border); }
  .count.active { color: var(--color-status-ok); }
  .count.errored { color: var(--color-status-error); }
  .auto-resume { display: flex; align-items: center; gap: 0.6rem; }
  .ar-label { font-size: 0.8rem; color: var(--color-text-secondary); }

  .group-title { font-size: 0.85rem; font-weight: 600; text-transform: uppercase; letter-spacing: 0.05em; margin: 1rem 0 0.5rem; color: var(--color-text-secondary); }
  .group-title.active { color: var(--color-status-ok); }
  .group-title.errored { color: var(--color-status-error); }
  .group-title.other { color: var(--color-status-warn); }

  .session-list { display: flex; flex-direction: column; gap: 0.5rem; }
  .session-card { background: var(--color-surface); border: 1px solid var(--color-border); border-radius: 0.5rem; padding: 0.75rem 1rem; }
  .session-head { display: flex; justify-content: space-between; align-items: center; gap: 0.5rem; }
  .session-name { font-weight: 600; color: var(--color-text-primary); font-size: 0.9rem; word-break: break-word; }
  .session-state { font-size: 0.7rem; padding: 0.1rem 0.45rem; border-radius: 9999px; border: 1px solid var(--color-border); color: var(--color-text-secondary); }
  .session-state.active { color: var(--color-status-ok); }
  .session-state.errored { color: var(--color-status-error); }
  .session-state.other { color: var(--color-status-warn); }
  .session-id { font-family: 'JetBrains Mono', monospace; font-size: 0.72rem; color: var(--color-text-muted); margin: 0.2rem 0 0.5rem; }
  .session-actions { display: flex; gap: 0.4rem; flex-wrap: wrap; }
  .session-actions button {
    background: none; border: 1px solid var(--color-border-hover); border-radius: 0.3rem;
    color: var(--color-accent); cursor: pointer; font-size: 0.72rem; padding: 0.2rem 0.55rem;
  }
  .session-actions button.danger { color: var(--color-status-error); }
  .session-actions button:disabled { opacity: 0.5; cursor: default; }
  .activity {
    margin-top: 0.6rem; background: var(--color-bg); border: 1px solid var(--color-border);
    border-radius: 0.4rem; padding: 0.6rem; font-family: 'JetBrains Mono', monospace;
    font-size: 0.72rem; color: var(--color-text-secondary); max-height: 240px; overflow: auto; white-space: pre-wrap;
  }
</style>
