<script lang="ts">
  // Why: DOC-39 §6.2 Phase 1 UI — "the status bar (readiness + budget)".
  // Both halves are pure wiring: `GET /sessions` + `GET /sessions/{id}/readiness`
  // landed with REST Slice 2 (#2983, squash 15156b42), and `GET
  // /sessions/{id}/budget` (issue #3015, PR #3042, squash 0bec593f) closed
  // the budget data gap this component previously called out — this is a
  // thin `fetch()` client over both, identically to `HealthPanel.svelte`
  // (no Tauri command, no client-side computation of daemon state beyond
  // the DOC-39 §4.5 AC-5.7 threshold classification in
  // `lib/context-budget.ts`).
  //
  // What: Polls `GET /sessions` then `GET /sessions/{id}/readiness` and
  // `GET /sessions/{id}/budget` for the session `pickActiveSession` selects,
  // every `POLL_MS`, via a Svelte 5 `$effect` that returns its own teardown
  // — no `onMount`/`onDestroy`. The teardown clears the interval AND aborts
  // one `AbortController` shared by every poll's `fetch()` calls;
  // `refresh()` also re-checks `signal.aborted` after each `await` before
  // writing `phase`/`session`/`readiness`/`budget`/`error`, so an in-flight
  // poll genuinely abandons its result on unmount (the network request is
  // cancelled, not merely ignored) rather than racing a disposed effect
  // scope. Renders one of four states: `daemon-unreachable` (the
  // `/sessions` fetch itself failed), `no-session` (daemon reachable, zero
  // sessions), `ready` (readiness + the budget slot — `recorded` renders the
  // real working-context %, `never_recorded` renders a labeled "no data
  // yet" rather than a fabricated `0%`), each as a `.statusbar` child —
  // never hidden, per DOC-39 §4.4 AC-4.2's "locked, not hidden" chrome
  // rule. A budget fetch failure (session reachable, budget route errors)
  // degrades the budget slot to "no data yet" without dropping the whole
  // status bar to `daemon-unreachable`, mirroring the existing readiness
  // partial-failure handling below.
  //
  // Test: `session-status.test.ts` covers `pickActiveSession`;
  // `context-budget.test.ts` covers `classifyBudget`/`budgetLabel`/
  // `budgetDotClass`/`budgetTitle`; `App.test.ts` asserts the DOC-39 §8.1
  // AC-18.1 DOM invariant (`.statusbar` is a sibling of `.body`, not a
  // descendant) that this component's root class participates in.
  import { apiBase } from '../lib/api-config';
  import {
    budgetDotClass,
    budgetLabel,
    budgetTitle,
    classifyBudget,
    type ContextBudgetQuery,
  } from '../lib/context-budget';
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
  let budget = $state<ContextBudgetQuery | null>(null);
  let error = $state<string | null>(null);

  async function refresh(signal: AbortSignal) {
    let base: string;
    try {
      base = await apiBase();
    } catch (e) {
      if (!signal.aborted) {
        phase = 'daemon-unreachable';
        error = e instanceof Error ? e.message : String(e);
      }
      return;
    }
    if (signal.aborted) return;

    let sessions: SessionSummary[];
    try {
      const res = await fetch(`${base}/sessions`, { signal });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const body = (await res.json()) as SessionListResponse;
      sessions = body.sessions;
    } catch (e) {
      if (!signal.aborted) {
        phase = 'daemon-unreachable';
        session = null;
        readiness = null;
        budget = null;
        error = e instanceof Error ? e.message : String(e);
      }
      return;
    }
    if (signal.aborted) return;

    const active = pickActiveSession(sessions);
    if (!active) {
      phase = 'no-session';
      session = null;
      readiness = null;
      budget = null;
      error = null;
      return;
    }
    session = active;

    try {
      const res = await fetch(`${base}/sessions/${active.id}/readiness`, { signal });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const body = (await res.json()) as ReadinessQuery;
      if (signal.aborted) return;
      readiness = body;
      phase = 'ready';
      error = null;
    } catch (e) {
      // The session itself is reachable (we just listed it) — a readiness
      // fetch failure here is a transient/partial error, not "no daemon".
      if (!signal.aborted) {
        phase = 'ready';
        readiness = null;
        error = e instanceof Error ? e.message : String(e);
      }
    }
    if (signal.aborted) return;

    try {
      const res = await fetch(`${base}/sessions/${active.id}/budget`, { signal });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const body = (await res.json()) as ContextBudgetQuery;
      if (signal.aborted) return;
      budget = body;
    } catch {
      // Same partial-failure discipline as readiness above: the session is
      // reachable, so a budget-route error degrades to "no data yet"
      // (`budget = null`) rather than dropping the whole status bar to
      // `daemon-unreachable`.
      if (!signal.aborted) budget = null;
    }
  }

  $effect(() => {
    // One controller for the whole mounted lifetime: every poll's two
    // `fetch()` calls share it, and aborting it once on teardown both
    // cancels any in-flight request AND flips every `signal.aborted` guard
    // inside `refresh()` — real cancellation, not a flag nothing reads.
    const controller = new AbortController();
    void refresh(controller.signal);
    const timer = setInterval(() => void refresh(controller.signal), POLL_MS);
    return () => {
      controller.abort();
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
      <span
        class={`flex items-center gap-1.5 ${classifyBudget(budget) === 'warn' ? 'text-status-error' : ''}`}
        title={budgetTitle(budget)}
      >
        <span class={`h-1.5 w-1.5 rounded-full ${budgetDotClass(budget)}`}></span>
        budget: {budgetLabel(budget)}
      </span>
    </div>
    {#if session}
      <span class="truncate text-trusty-text/40">session {session.id.slice(0, 8)}</span>
    {/if}
  {/if}
</footer>
