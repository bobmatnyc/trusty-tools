<script lang="ts">
  // Why: DOC-39 §6.2 Phase 1 UI — "the status bar (readiness + budget)",
  // extended per the issue #3153 shell rebuild's segment list: "workstream
  // state · PROJECT binding · SEARCH readiness · MEM · AGENTS mode ·
  // TOKENS · COST". `GET /sessions` + `GET /sessions/{id}/readiness`
  // landed with REST Slice 2 (#2983, squash 15156b42), `GET
  // /sessions/{id}/budget` (issue #3015, PR #3042) supplies TOKENS, and
  // this rebuild adds one more chained fetch — `GET /sessions/{id}` — for
  // the fields already used elsewhere (`WorkstreamActivity.svelte`) that this
  // bar didn't previously read: `project` (the PROJECT segment) and `agent`
  // (the AGENTS-mode segment). MEM and COST have no daemon route yet
  // (memory-palace stats need #3181's aggregation; live cost is #3254) —
  // both render an honest static "n/a"/stub rather than a fabricated
  // value, same discipline `budgetLabel`'s "no data yet" already
  // established. `HealthPanel.svelte` is REMOVED (build brief: "fold
  // daemon-unreachable signal into status bar") — this component's own
  // `daemon-unreachable` phase already reports the identical fact.
  //
  // What: Polls `GET /sessions`, then `GET /sessions/{id}`,
  // `GET /sessions/{id}/readiness`, and `GET /sessions/{id}/budget` for the
  // session `pickActiveSession` selects, every `POLL_MS`, via a Svelte 5
  // `$effect` that returns its own teardown — no `onMount`/`onDestroy`. The
  // teardown clears the interval AND aborts one `AbortController` shared by
  // every poll's `fetch()` calls; `refresh()` also re-checks
  // `signal.aborted` after each `await` before writing state, so an
  // in-flight poll genuinely abandons its result on unmount. Renders one of
  // four phases: `daemon-unreachable` (the `/sessions` fetch itself
  // failed), `no-session` (daemon reachable, zero sessions), `ready` (all
  // seven segments — each as a `.statusbar` child, never hidden, per
  // DOC-39 §4.4 AC-4.2's "locked, not hidden" chrome rule). A detail/
  // readiness/budget fetch failure (session reachable, one sub-route
  // errors) degrades that ONE segment to "no data yet"/"n/a" without
  // dropping the whole status bar to `daemon-unreachable` — the existing
  // readiness partial-failure discipline, now applied uniformly to every
  // segment.
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
    type SessionDetail,
    type SessionListResponse,
    type SessionSummary,
  } from '../lib/session-status';

  const POLL_MS = 5000;

  type Phase = 'connecting' | 'daemon-unreachable' | 'no-session' | 'ready';

  let phase = $state<Phase>('connecting');
  let session = $state<SessionSummary | null>(null);
  let detail = $state<SessionDetail | null>(null);
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
        detail = null;
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
      detail = null;
      readiness = null;
      budget = null;
      error = null;
      return;
    }
    session = active;

    try {
      const res = await fetch(`${base}/sessions/${active.id}`, { signal });
      if (res.status === 404) {
        // Reachable daemon, just-listed session vanished mid-poll — same
        // partial-case handling every other poller in this codebase applies.
        if (!signal.aborted) {
          phase = 'no-session';
          session = null;
          detail = null;
          readiness = null;
          budget = null;
          error = null;
        }
        return;
      }
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const body = (await res.json()) as SessionDetail;
      if (!signal.aborted) detail = body;
    } catch {
      // PROJECT/AGENTS-mode segments degrade to "n/a" — the session is
      // reachable (we just listed it), so this is a partial failure, not
      // "no daemon".
      if (!signal.aborted) detail = null;
    }
    if (signal.aborted) return;

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
  class="statusbar flex items-center justify-between gap-4 border-t border-trusty-border bg-trusty-raised px-4 py-2 font-mono text-[11px] uppercase tracking-wide text-trusty-text-secondary"
>
  {#if phase === 'connecting'}
    <span class="text-trusty-text-muted">connecting…</span>
  {:else if phase === 'daemon-unreachable'}
    <span class="flex items-center gap-1.5 text-status-error">
      <span class="h-1.5 w-1.5 rounded-full bg-status-error"></span>
      daemon unreachable{error ? ` — ${error}` : ''}
    </span>
  {:else if phase === 'no-session'}
    <span class="flex items-center gap-1.5 text-trusty-text-muted">
      <span class="h-1.5 w-1.5 rounded-full bg-status-neutral"></span>
      no active workstream
    </span>
  {:else}
    <div class="flex items-center gap-4 overflow-x-auto">
      <span title={session?.id}>workstream: {session?.status ?? 'unknown'}</span>
      <span title={detail?.project ?? session?.project ?? 'no project bound yet'}>
        project: {detail?.project ?? session?.project ?? 'projectless'}
      </span>
      <span class="flex items-center gap-1.5" title={session?.id}>
        <span class={`h-1.5 w-1.5 rounded-full ${readinessDotClass()}`}></span>
        search: {readinessLabel()}
      </span>
      <span title="Memory-palace stats need #3181's service-status aggregation — not built yet">
        mem: n/a
      </span>
      <span>agents: {detail?.agent ?? 'n/a'}</span>
      <span
        class={`flex items-center gap-1.5 ${classifyBudget(budget) === 'warn' ? 'text-status-error' : ''}`}
        title={budgetTitle(budget)}
      >
        <span class={`h-1.5 w-1.5 rounded-full ${budgetDotClass(budget)}`}></span>
        tokens: {budgetLabel(budget)}
      </span>
      <span title="Live cost tracking is issue #3254 — not built yet">cost: —</span>
    </div>
    {#if session}
      <span class="truncate normal-case text-trusty-text-muted" title={session.id}
        >id {session.id.slice(0, 8)}</span
      >
    {/if}
  {/if}
</footer>
