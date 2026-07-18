<script lang="ts">
  // Why: DOC-39 §4.7 (SPEC-TCUI, "'Search' is two different things") defines
  // the **Search tab (10d)** as the audit trail of agent search operations —
  // explicitly NOT a search box (AC-7.1: "no input field"). Literal find
  // (⌘K) is a *different*, not-yet-built surface scoped to the Project page
  // (AC-7.3/7.4), out of scope here.
  //
  // AC-7.2 requires every row to show lane badge, query, hit count, latency,
  // **requesting agent**, and age. PR #3085 shipped this tab as an honest
  // shell — the column headers with a labeled gap notice in place of rows —
  // because `Event::SearchPerformed`/`Event::MemoryRecalled` were SSE-only
  // with no REST snapshot route (issue #3072). That route now exists
  // (`GET /sessions/{id}/search-audit`, PR #3107) backed by an
  // always-retained, 200-record-capped `SessionEntry.search_audit` list, so
  // this slice replaces the gap notice with real rows polled the same way
  // `StatusBar.svelte`/`SessionMonitor.svelte` poll their own REST routes —
  // still a thin REST client, never an SSE consumer.
  //
  // What: Polls `GET /sessions` to pick the active session
  // (`pickActiveSession`), then `GET /sessions/{id}/search-audit` for that
  // session, every `POLL_MS` — identical `$effect`/`AbortController`/
  // `setInterval` shape to the sibling components. A `404` on the audit
  // fetch (session vanished between the two calls in the same `refresh()`)
  // is treated as `no-session`, same as `SessionMonitor.svelte`'s handling
  // of its own chained fetches. A non-200 or top-level-malformed body
  // degrades to an `audit unavailable` row rather than throwing — the
  // fetched body's shape is verified with `parseSearchAuditResponse`
  // (`lib/search-audit.ts`, mirroring `create-session.ts::isDirListing`)
  // before it becomes reactive state; no bare `as` assertion on
  // network-derived data. A single malformed RECORD (or a future
  // `SearchAuditRecord` variant this build doesn't recognize) does NOT take
  // down the whole table (issue #3111 MEDIUM) — `parseSearchAuditResponse`
  // filters to the trustworthy subset and reports how many rows it dropped,
  // rendered as a visible "N rows omitted" notice alongside the known-good
  // rows rather than silently or wholesale. Rows render in the order the
  // daemon returns them (oldest first, newest last — the API doc's own
  // ordering), so the most recent search lands where the eye naturally
  // finishes reading, same convention as `transcript.ts::selectTranscriptTail`.
  // `Recall` rows have no `lane`/`latency_ms` on the wire;
  // `auditLaneLabel`/`auditLatencyLabel` (`lib/search-audit.ts`) normalize
  // both variants onto the shared six columns without fabricating fields
  // neither carries. A second, local `now` tick (no network call) redisplays
  // each row's age every second via `transcript.ts::formatElapsed`, same
  // pattern as `SessionMonitor.svelte`.
  //
  // Test: `SearchTab.test.ts` covers the four connection phases, the
  // populated/empty/partially-valid/malformed audit states, a poll-recovery
  // sequence, and asserts no `<input>`/`<textarea>` ever renders (AC-7.1).
  import { apiBase } from '../lib/api-config';
  import {
    auditHitsLabel,
    auditLaneLabel,
    auditLatencyLabel,
    parseSearchAuditResponse,
    type SearchAuditRecord,
  } from '../lib/search-audit';
  import {
    pickActiveSession,
    type SessionListResponse,
    type SessionSummary,
  } from '../lib/session-status';
  import { formatElapsed } from '../lib/transcript';

  const POLL_MS = 5000; // matches StatusBar.svelte / SessionMonitor.svelte poll cadence
  const TICK_MS = 1000; // local age-redisplay tick only — no network call

  type Phase = 'connecting' | 'daemon-unreachable' | 'no-session' | 'active';

  let phase = $state<Phase>('connecting');
  let session = $state<SessionSummary | null>(null);
  let error = $state<string | null>(null);
  let auditRows = $state<SearchAuditRecord[]>([]);
  let auditOmittedCount = $state(0);
  let auditError = $state<string | null>(null);
  let now = $state(Date.now());

  async function refresh(signal: AbortSignal) {
    let base: string;
    try {
      base = await apiBase();
    } catch (e) {
      if (!signal.aborted) {
        phase = 'daemon-unreachable';
        session = null;
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
        error = e instanceof Error ? e.message : String(e);
      }
      return;
    }
    if (signal.aborted) return;

    const active = pickActiveSession(sessions);
    if (!active) {
      phase = 'no-session';
      session = null;
      auditRows = [];
      auditOmittedCount = 0;
      auditError = null;
      error = null;
      return;
    }
    session = active;
    phase = 'active';
    error = null;

    try {
      const res = await fetch(`${base}/sessions/${active.id}/search-audit`, { signal });
      if (res.status === 404) {
        // Reachable daemon, just-listed session vanished mid-poll — same
        // partial-case handling as SessionMonitor.svelte's chained fetches.
        if (!signal.aborted) {
          phase = 'no-session';
          session = null;
          auditRows = [];
          auditOmittedCount = 0;
          auditError = null;
        }
        return;
      }
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const body: unknown = await res.json();
      if (signal.aborted) return;
      const parsed = parseSearchAuditResponse(body);
      if (!parsed) {
        // Top-level shape failure only (not an object, or `search_audit`
        // isn't even an array) — nothing here can be trusted or filtered.
        auditRows = [];
        auditOmittedCount = 0;
        auditError = 'malformed response from daemon';
        return;
      }
      // A record-level failure (or an unrecognized future `kind`) never
      // takes down the whole table (issue #3111) — render whatever subset
      // IS trustworthy plus an honest omitted-count, not "audit unavailable".
      auditRows = parsed.records;
      auditOmittedCount = parsed.omittedCount;
      auditError = null;
    } catch (e) {
      if (!signal.aborted) {
        auditRows = [];
        auditOmittedCount = 0;
        auditError = e instanceof Error ? e.message : String(e);
      }
    }
  }

  $effect(() => {
    // One controller for the whole mounted lifetime — same shape as
    // StatusBar.svelte/SessionMonitor.svelte: aborting once on teardown both
    // cancels any in-flight request and flips every `signal.aborted` guard
    // inside `refresh()`.
    const controller = new AbortController();
    void refresh(controller.signal);
    const timer = setInterval(() => void refresh(controller.signal), POLL_MS);
    return () => {
      controller.abort();
      clearInterval(timer);
    };
  });

  $effect(() => {
    // Independent of the network poll: a pure local redisplay tick so each
    // row's "age" column advances smoothly without waiting on POLL_MS. No
    // fetch, no AbortController needed — clearing the interval is the whole
    // teardown (mirrors SessionMonitor.svelte's elapsed-time tick).
    const timer = setInterval(() => {
      now = Date.now();
    }, TICK_MS);
    return () => clearInterval(timer);
  });
</script>

<section class="mt-4 rounded-lg border border-trusty-border bg-trusty-surface/60 p-4">
  <h2 class="text-sm font-semibold text-trusty-text">search</h2>
  <p class="mt-1 text-xs text-trusty-text/50">
    "Search" here isn't a box you type in — this tab is the audit trail of the searches
    your agents performed.
  </p>

  {#if phase === 'connecting'}
    <p class="mt-2 text-xs text-trusty-text/50">connecting&hellip;</p>
  {:else if phase === 'daemon-unreachable'}
    <p class="mt-2 flex items-center gap-1.5 text-xs text-status-error">
      <span class="h-1.5 w-1.5 rounded-full bg-status-error"></span>
      daemon unreachable{error ? ` — ${error}` : ''}
    </p>
  {:else if phase === 'no-session'}
    <p class="mt-2 flex items-center gap-1.5 text-xs text-trusty-text/60">
      <span class="h-1.5 w-1.5 rounded-full bg-status-neutral"></span>
      no active session — nothing to audit yet
    </p>
  {:else if session}
    <div class="mt-3 overflow-x-auto">
      <table class="w-full text-left font-mono text-xs">
        <thead>
          <tr class="text-trusty-text/40">
            <th class="pb-1 pr-3 font-normal">lane</th>
            <th class="pb-1 pr-3 font-normal">query</th>
            <th class="pb-1 pr-3 font-normal">hits</th>
            <th class="pb-1 pr-3 font-normal">latency</th>
            <th class="pb-1 pr-3 font-normal">agent</th>
            <th class="pb-1 font-normal">age</th>
          </tr>
        </thead>
        <tbody>
          {#if auditError}
            <tr>
              <td colspan="6" class="pt-2 text-status-error">
                audit unavailable — {auditError}
              </td>
            </tr>
          {:else if auditRows.length === 0 && auditOmittedCount === 0}
            <tr>
              <td colspan="6" class="pt-2 text-trusty-text/40">no searches recorded yet</td>
            </tr>
          {:else}
            {#each auditRows as record, i (`${record.at}-${record.agent_id}-${i}`)}
              <tr class="text-trusty-text/80">
                <td class="py-0.5 pr-3">{auditLaneLabel(record)}</td>
                <td class="py-0.5 pr-3 truncate" title={record.query}>{record.query}</td>
                <td class="py-0.5 pr-3">{auditHitsLabel(record)}</td>
                <td class="py-0.5 pr-3">{auditLatencyLabel(record)}</td>
                <td class="py-0.5 pr-3">{record.agent}</td>
                <td class="py-0.5">{formatElapsed(record.at, now)}</td>
              </tr>
            {/each}
            {#if auditOmittedCount > 0}
              <tr>
                <td
                  colspan="6"
                  class="pt-2 text-trusty-text/40"
                  title="One or more search-audit records from the daemon didn't match a known shape (malformed or an unrecognized future kind) and were left out rather than crashing the tab."
                >
                  {auditOmittedCount}
                  {auditOmittedCount === 1 ? 'row' : 'rows'} omitted (unrecognized record shape)
                </td>
              </tr>
            {/if}
          {/if}
        </tbody>
      </table>
    </div>
  {/if}
</section>
