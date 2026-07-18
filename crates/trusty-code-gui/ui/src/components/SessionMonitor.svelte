<script lang="ts">
  // Why: DOC-39 §4.6 (SPEC-TCUI-06) describes the 8b monitor lifecycle as a
  // live docked-rail search/memory-recall widget that settles into an
  // inline card preserving `lane`/`query`/`hit_count`/`latency_ms`
  // (`Event::SearchPerformed`/`Event::MemoryRecalled` in
  // `crates/trusty-code/src/events.rs`). Those events are emitted ONLY on
  // the SSE stream (`GET /sessions/{id}/events`) — there is no REST
  // snapshot route, no "settled list" endpoint, and no prior PR in this
  // repo has established an SSE-consumption pattern in the Svelte client.
  // Building the literal AC-6.3 card in this slice would mean the GUI
  // becomes an SSE consumer buffering an unbounded per-session event log
  // client-side — real state/derivation the thin-client architecture
  // discourages introducing ad hoc in a Phase-1 slice.
  //
  // Per explicit scoping direction, this component instead ships a SESSION
  // MONITOR card: an active-session summary — status, task, elapsed time, a
  // recent transcript tail, and a cancel action — built entirely from REST
  // routes that already exist (`GET /sessions`, `GET /sessions/{id}`,
  // `GET /sessions/{id}/transcript`, `POST /sessions/{id}/cancel`, REST
  // Slices 2-3, #2983). This is an honest, legitimate Phase-1 deliverable,
  // but it does NOT fulfil AC-6.3's literal "live search/recall monitor
  // settling into an inline card" requirement. That gap is rendered as a
  // labeled, non-hidden notice below (mirroring exactly how
  // `StatusBar.svelte` labels its missing-budget-RPC gap).
  //
  // UPDATE (issue #3027 closed, tracked onward as #3108): the underlying
  // REST gap this comment originally described — `SearchPerformed`/
  // `MemoryRecalled` being SSE-only with no snapshot route — is now closed
  // (`GET /sessions/{id}/search-audit`, issue #3072, PR #3107). The sibling
  // `SearchTab.svelte` (10d) consumes it for its full audit trail (AC-7.2).
  // This component's own settled-card half of AC-6.3 has NOT been wired to
  // that route yet — that remaining, purely client-side gap is tracked as
  // issue #3108 rather than #3027 (now closed for the Search tab reason).
  //
  // What: Polls `GET /sessions` then `GET /sessions/{id}` +
  // `GET /sessions/{id}/transcript` for the session `pickActiveSession`
  // selects, every `POLL_MS` — identical `$effect`/`AbortController`/
  // `setInterval` shape to `StatusBar.svelte` (one controller for the whole
  // mounted lifetime, `signal.aborted` re-checked after every `await`
  // before any state write, teardown aborts + clears the interval). A
  // second, independent `$effect` ticks a local `now` every second purely
  // for elapsed-time redisplay (`formatElapsed`, `lib/transcript.ts`) — no
  // network call, own teardown. Renders one of four states
  // (`connecting`/`daemon-unreachable`/`no-session`/`ready`), same
  // never-hidden discipline as `StatusBar` (DOC-39 AC-4.2). A `GET
  // /sessions/{id}` 404 immediately after a successful `GET /sessions`
  // (session vanished mid-poll) is treated as `no-session`, not
  // `daemon-unreachable` — the daemon answered fine, it just no longer has
  // that session. Cancel requires a two-step in-card confirm (no
  // `window.confirm()` — not testable, not visually consistent with the
  // design system) and forces an immediate `refresh()` on success rather
  // than waiting for the next poll tick.
  //
  // Test: `lib/transcript.test.ts` covers the pure tail/elapsed helpers;
  // `App.test.ts` pins that this component mounts inside `.body`.
  import { apiBase } from '../lib/api-config';
  import {
    pickActiveSession,
    TERMINAL_SESSION_STATUSES,
    type SessionDetail,
    type SessionListResponse,
    type SessionSummary,
  } from '../lib/session-status';
  import { formatElapsed, selectTranscriptTail, type TranscriptRecord } from '../lib/transcript';

  const POLL_MS = 5000; // matches StatusBar.svelte's poll cadence
  const TICK_MS = 1000; // local redisplay tick only — no network call

  /** Tracks the future AC-6.3 gap (see the Why block above). */
  const SEARCH_RECALL_GAP_ISSUE = 'https://github.com/bobmatnyc/trusty-tools/issues/3108';

  type Phase = 'connecting' | 'daemon-unreachable' | 'no-session' | 'ready';
  type CancelPhase = 'idle' | 'confirming' | 'cancelling';

  let phase = $state<Phase>('connecting');
  let session = $state<SessionDetail | null>(null);
  let transcript = $state<TranscriptRecord | null>(null);
  let error = $state<string | null>(null);
  let cancelPhase = $state<CancelPhase>('idle');
  let now = $state(Date.now());

  let pollController: AbortController | null = null;

  async function refresh(signal: AbortSignal) {
    let base: string;
    try {
      base = await apiBase();
    } catch (e) {
      if (!signal.aborted) {
        phase = 'daemon-unreachable';
        session = null;
        transcript = null;
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
        transcript = null;
        error = e instanceof Error ? e.message : String(e);
      }
      return;
    }
    if (signal.aborted) return;

    const active = pickActiveSession(sessions);
    if (!active) {
      phase = 'no-session';
      session = null;
      transcript = null;
      error = null;
      cancelPhase = 'idle';
      return;
    }
    if (session?.id !== active.id) {
      // The active session changed under us — drop any pending confirm
      // rather than let it apply to a different session on the next click.
      cancelPhase = 'idle';
    }

    try {
      const res = await fetch(`${base}/sessions/${active.id}`, { signal });
      if (res.status === 404) {
        // Reachable daemon, just-listed session vanished mid-poll — a
        // partial "no longer here" case, not a daemon-unreachable one.
        if (!signal.aborted) {
          phase = 'no-session';
          session = null;
          transcript = null;
          error = null;
          cancelPhase = 'idle';
        }
        return;
      }
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const detail = (await res.json()) as SessionDetail;
      if (signal.aborted) return;
      session = detail;
      phase = 'ready';
      error = null;
    } catch (e) {
      // Session was reachable in the list moments ago — a detail-fetch
      // failure here is transient/partial, not "no daemon" (mirrors
      // StatusBar's readiness partial-error handling).
      if (!signal.aborted) {
        error = e instanceof Error ? e.message : String(e);
      }
      return;
    }
    if (signal.aborted) return;

    try {
      const res = await fetch(`${base}/sessions/${active.id}/transcript`, { signal });
      if (res.status === 404) {
        // Same "reachable daemon, session vanished mid-poll" case as the
        // detail fetch above — the session could disappear between the two
        // calls in this same refresh(), and without this branch the card
        // would stay phase='ready' with a stale `session` (Cancel button
        // included) showing a raw HTTP 404 as `error` until the next poll.
        if (!signal.aborted) {
          phase = 'no-session';
          session = null;
          transcript = null;
          error = null;
          cancelPhase = 'idle';
        }
        return;
      }
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const body = (await res.json()) as TranscriptRecord;
      if (!signal.aborted) transcript = body;
    } catch (e) {
      if (!signal.aborted) {
        transcript = null;
        error = e instanceof Error ? e.message : String(e);
      }
    }
  }

  async function doCancel() {
    if (!session || !pollController) return;
    const id = session.id;
    const signal = pollController.signal;
    cancelPhase = 'cancelling';
    try {
      const base = await apiBase();
      if (signal.aborted) return;
      const res = await fetch(`${base}/sessions/${id}/cancel`, { method: 'POST', signal });
      if (signal.aborted) return;
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      error = null;
    } catch (e) {
      if (!signal.aborted) {
        error = e instanceof Error ? e.message : String(e);
      }
    }
    if (!signal.aborted) cancelPhase = 'idle';
    // Reflect the new status immediately rather than waiting for the next
    // poll tick — cancellation is cooperative, not instantaneous, but the
    // UI should show whatever the daemon reports right away.
    if (!signal.aborted && pollController) void refresh(pollController.signal);
  }

  $effect(() => {
    // One controller for the whole mounted lifetime — same shape as
    // StatusBar.svelte: every poll's fetches share it, and aborting once on
    // teardown both cancels any in-flight request and flips every
    // `signal.aborted` guard inside `refresh()`.
    const controller = new AbortController();
    pollController = controller;
    void refresh(controller.signal);
    const timer = setInterval(() => void refresh(controller.signal), POLL_MS);
    return () => {
      controller.abort();
      if (pollController === controller) pollController = null;
      clearInterval(timer);
    };
  });

  $effect(() => {
    // Independent of the network poll: a pure local redisplay tick so
    // "elapsed" advances smoothly without waiting on POLL_MS. No fetch, no
    // AbortController needed — clearing the interval is the whole teardown.
    const timer = setInterval(() => {
      now = Date.now();
    }, TICK_MS);
    return () => clearInterval(timer);
  });

  let tailEntries = $derived(transcript ? selectTranscriptTail(transcript.turns) : []);
  let canCancel = $derived(session !== null && !TERMINAL_SESSION_STATUSES.has(session.status));

  function statusDotClass(status: string): string {
    switch (status) {
      case 'running':
        return 'bg-status-warn'; // in progress
      case 'finished':
        return 'bg-status-ok';
      case 'failed':
      case 'deadline_exceeded':
        return 'bg-status-error';
      case 'cancelled':
      case 'created':
      default:
        return 'bg-status-neutral';
    }
  }
</script>

<section class="mt-4 rounded-lg border border-trusty-border bg-trusty-surface/60 p-4">
  <h2 class="text-sm font-semibold text-trusty-text">session monitor</h2>

  {#if phase === 'connecting'}
    <p class="mt-2 text-xs text-trusty-text/50">connecting…</p>
  {:else if phase === 'daemon-unreachable'}
    <p class="mt-2 flex items-center gap-1.5 text-xs text-status-error">
      <span class="h-1.5 w-1.5 rounded-full bg-status-error"></span>
      daemon unreachable{error ? ` — ${error}` : ''}
    </p>
  {:else if phase === 'no-session'}
    <p class="mt-2 flex items-center gap-1.5 text-xs text-trusty-text/60">
      <span class="h-1.5 w-1.5 rounded-full bg-status-neutral"></span>
      no active session to monitor
    </p>
  {:else if session}
    <div class="mt-2 flex items-center gap-2 text-xs">
      <span class={`h-1.5 w-1.5 rounded-full ${statusDotClass(session.status)}`}></span>
      <span class="font-medium text-trusty-text">{session.status}</span>
      <span class="text-trusty-text/30">·</span>
      <span class="text-trusty-text/60">{formatElapsed(session.created_at, now)}</span>
      <span class="text-trusty-text/30">·</span>
      <span class="truncate text-trusty-text/40" title={session.id}>{session.id.slice(0, 8)}</span>
    </div>

    <p class="mt-2 truncate text-sm text-trusty-text" title={session.task}>{session.task}</p>

    <div class="mt-3 space-y-1 text-xs">
      {#if tailEntries.length === 0}
        <p class="text-trusty-text/40">no turns recorded yet</p>
      {:else}
        {#each tailEntries as entry, i (i)}
          <p class="text-trusty-text/70">
            <span class="font-medium text-trusty-text/90">{entry.agent}</span>: {entry.preview}
          </p>
        {/each}
      {/if}
    </div>

    {#if error}
      <p class="mt-2 text-xs text-status-error">{error}</p>
    {/if}

    <div class="mt-3 flex items-center gap-2">
      {#if canCancel}
        {#if cancelPhase === 'idle'}
          <button
            type="button"
            class="rounded border border-trusty-border px-2 py-1 text-xs text-trusty-text/70 hover:bg-trusty-border/20"
            onclick={() => (cancelPhase = 'confirming')}
          >
            Cancel
          </button>
        {:else if cancelPhase === 'confirming'}
          <span class="text-xs text-trusty-text/60">Confirm cancel?</span>
          <button
            type="button"
            class="rounded border border-status-error px-2 py-1 text-xs text-status-error hover:bg-status-error/10"
            onclick={doCancel}
          >
            Confirm cancel
          </button>
          <button
            type="button"
            class="rounded border border-trusty-border px-2 py-1 text-xs text-trusty-text/70 hover:bg-trusty-border/20"
            onclick={() => (cancelPhase = 'idle')}
          >
            Never mind
          </button>
        {:else}
          <span class="text-xs text-trusty-text/50">cancelling…</span>
        {/if}
      {/if}
    </div>

    <p
      class="mt-3 text-[11px] text-trusty-text/30"
      title={`DOC-39 §4.6 AC-6.3's live search/memory-recall monitor (lane/query/hit_count/latency, docked-rail → inline settle) is not implemented here — GET /sessions/{id}/search-audit now exists (issue #3072) but this card has not been wired to it yet. Tracked at ${SEARCH_RECALL_GAP_ISSUE}`}
    >
      search/recall monitor (AC-6.3): not yet implemented — see issue #3108
    </p>
  {/if}
</section>
