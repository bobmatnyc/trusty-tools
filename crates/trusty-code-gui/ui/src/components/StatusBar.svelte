<script lang="ts">
  // Why: DOC-39 §6.2 Phase 1 UI — "the status bar (readiness + budget)".
  // Readiness is pure wiring: `GET /sessions` + `GET /sessions/{id}/readiness`
  // landed with REST Slice 2 (#2983, squash 15156b42) and this component is
  // a thin `fetch()` client over them, identically to `HealthPanel.svelte`
  // (no Tauri command, no client-side computation of daemon state).
  //
  // Budget is a DATA GAP, not a rendering gap: `Event::ContextBudget`
  // (`crates/trusty-code/src/events.rs`) is emitted on the SSE stream but
  // never cached on the session the way `IndexReadinessSnapshot` is
  // (`SessionRegistry::record_context_budget` calls `self.record(...)` only —
  // compare `record_index_readiness`, which also writes `entry.readiness`).
  // There is today no `session.get_context_budget` RPC and therefore no
  // `GET /sessions/{id}/budget` REST route to poll. Per the layer-priority
  // rule this PR does NOT add one; the budget slot renders a labeled
  // "unavailable" state instead of being silently omitted (DOC-39 AC-5.1:
  // "an invisible budget is not a budget") and the gap is called out in the
  // PR body as a REST-slice follow-up.
  //
  // What: Polls `GET /sessions` then `GET /sessions/{id}/readiness` for the
  // session `pickActiveSession` selects, every `POLL_MS`, via a Svelte 5
  // `$effect` that returns its own teardown (clears the interval and marks
  // in-flight polls stale on unmount — no `onMount`/`onDestroy`). Renders one
  // of four states: `daemon-unreachable` (the `/sessions` fetch itself
  // failed), `no-session` (daemon reachable, zero sessions), `ready`
  // (readiness + the budget placeholder), each as a `.statusbar` child —
  // never hidden, per DOC-39 §4.4 AC-4.2's "locked, not hidden" chrome rule.
  //
  // Test: `session-status.test.ts` covers `pickActiveSession`;
  // `App.test.ts` asserts the DOC-39 §8.1 AC-18.1 DOM invariant
  // (`.statusbar` is a sibling of `.body`, not a descendant) that this
  // component's root class participates in.
  import { apiBase } from '../lib/api-config';
  import {
    pickActiveSession,
    type ReadinessQuery,
    type SessionListResponse,
    type SessionSummary,
  } from '../lib/session-status';

  const POLL_MS = 5000;

  type Phase = 'connecting' | 'daemon-unreachable' | 'no-session' | 'ready';

  let phase = $state<Phase>('connecting');
  let session = $state<SessionSummary | null>(null);
  let readiness = $state<ReadinessQuery | null>(null);
  let error = $state<string | null>(null);

  async function refresh() {
    let base: string;
    try {
      base = await apiBase();
    } catch (e) {
      phase = 'daemon-unreachable';
      error = e instanceof Error ? e.message : String(e);
      return;
    }

    let sessions: SessionSummary[];
    try {
      const res = await fetch(`${base}/sessions`);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const body = (await res.json()) as SessionListResponse;
      sessions = body.sessions;
    } catch (e) {
      phase = 'daemon-unreachable';
      session = null;
      readiness = null;
      error = e instanceof Error ? e.message : String(e);
      return;
    }

    const active = pickActiveSession(sessions);
    if (!active) {
      phase = 'no-session';
      session = null;
      readiness = null;
      error = null;
      return;
    }
    session = active;

    try {
      const res = await fetch(`${base}/sessions/${active.id}/readiness`);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      readiness = (await res.json()) as ReadinessQuery;
      phase = 'ready';
      error = null;
    } catch (e) {
      // The session itself is reachable (we just listed it) — a readiness
      // fetch failure here is a transient/partial error, not "no daemon".
      phase = 'ready';
      readiness = null;
      error = e instanceof Error ? e.message : String(e);
    }
  }

  $effect(() => {
    let cancelled = false;
    void (async () => {
      if (!cancelled) await refresh();
    })();
    const timer = setInterval(() => {
      if (!cancelled) void refresh();
    }, POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  });

  function readinessLabel(): string {
    if (!readiness) return 'unknown';
    if (readiness.status === 'never_probed') return 'never probed';
    return readiness.state;
  }

  function readinessDotClass(): string {
    if (!readiness || readiness.status === 'never_probed') return 'bg-status-neutral';
    switch (readiness.state) {
      case 'ready':
        return 'bg-status-ok';
      case 'warming':
        return 'bg-status-warn';
      default:
        return 'bg-status-error';
    }
  }
</script>

<footer
  class="statusbar flex items-center justify-between gap-4 border-t border-trusty-border bg-trusty-surface/80 px-4 py-2 font-mono text-xs"
>
  {#if phase === 'connecting'}
    <span class="text-trusty-text/50">connecting…</span>
  {:else if phase === 'daemon-unreachable'}
    <span class="flex items-center gap-1.5 text-status-error">
      <span class="h-1.5 w-1.5 rounded-full bg-status-error"></span>
      daemon unreachable{error ? ` — ${error}` : ''}
    </span>
  {:else if phase === 'no-session'}
    <span class="flex items-center gap-1.5 text-trusty-text/60">
      <span class="h-1.5 w-1.5 rounded-full bg-status-neutral"></span>
      no active session
    </span>
  {:else}
    <div class="flex items-center gap-4">
      <span class="flex items-center gap-1.5" title={session?.id}>
        <span class={`h-1.5 w-1.5 rounded-full ${readinessDotClass()}`}></span>
        readiness: {readinessLabel()}
      </span>
      <span class="text-trusty-text/40" title="Budget: no poll-able REST route yet (Event::ContextBudget is SSE-only and not cached on the session) — tracked as a REST-slice follow-up">
        budget: unavailable
      </span>
    </div>
    {#if session}
      <span class="truncate text-trusty-text/40">session {session.id.slice(0, 8)}</span>
    {/if}
  {/if}
</footer>
