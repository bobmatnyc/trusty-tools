<script lang="ts">
  // Why: DOC-39 §4.7 (SPEC-TCUI, "'Search' is two different things") defines
  // the **Search tab (10d)** as the audit trail of agent search operations —
  // explicitly NOT a search box (AC-7.1: "no input field"). Literal find
  // (⌘K) is a *different*, not-yet-built surface scoped to the Project page
  // (AC-7.3/7.4), out of scope here.
  //
  // AC-7.2 requires every row to show lane badge, query, hit count, latency,
  // **requesting agent**, and age. The underlying data exists —
  // `Event::SearchPerformed`/`Event::MemoryRecalled`
  // (`crates/trusty-code/src/events.rs`) already carry `lane`/`query`/
  // `hit_count`/`hits`/`latency_ms`/`agent`/`agent_id` — but per this PR's
  // architecture rule, this tab must be a thin REST client, never an SSE
  // consumer buffering an unbounded per-session event log client-side, and
  // never a direct `POST /rpc` caller (that would bypass the REST
  // resource-gateway pattern `serve/rest/mod.rs` establishes). Both events
  // are emitted ONLY on the SSE stream (`GET /sessions/{id}/events`) with no
  // REST snapshot/list route and no persisted per-session accumulation
  // anywhere in the registry — so there is no legal REST data source for
  // AC-7.2's rows today. This is the SAME underlying gap `SessionMonitor.svelte`
  // already calls out for the 8b inline monitor (issue #3027); the audit-list
  // half (10d, this component) is tracked separately as issue #3072, which
  // proposes the exact fix: a new `GET /sessions/{id}/search-audit` REST
  // route backed by an always-retained `SessionEntry.search_audit` list
  // (mirroring the #2962 agents-map precedent), appended from the same
  // `SessionRegistry::record_search_performed`/`record_memory_recalled` path
  // that already emits the SSE event.
  //
  // What: Ships the honest Phase-1 shell: the AC-7.1 explanatory banner (no
  // input field — literally none is rendered, so AC-7.1 holds by
  // construction), the AC-7.2 column headers (Lane / Query / Hits / Latency /
  // Agent / Age) as static table structure, and a labeled, non-hidden gap
  // notice in place of rows (same treatment as `StatusBar`'s `budget:
  // unavailable` span and `SessionMonitor`'s AC-6.3 notice). Polls
  // `GET /sessions` only (identical `$effect`/`AbortController`/
  // `setInterval` shape to `StatusBar.svelte`/`SessionMonitor.svelte`) to
  // distinguish `connecting`/`daemon-unreachable`/`no-session` from a real
  // active session — once a session exists the gap notice explains why the
  // table is empty (no data source) rather than implying zero searches ran.
  //
  // Test: `SearchTab.test.ts` covers the four phases and asserts no
  // `<input>`/`<textarea>` renders (AC-7.1); `App.test.ts` pins that this
  // component mounts inside `.body`.
  import { apiBase } from '../lib/api-config';
  import {
    pickActiveSession,
    type SessionListResponse,
    type SessionSummary,
  } from '../lib/session-status';

  const POLL_MS = 5000; // matches StatusBar.svelte / SessionMonitor.svelte poll cadence

  /** Tracks the REST-snapshot-route gap this tab cannot fill client-side. */
  const SEARCH_AUDIT_GAP_ISSUE = 'https://github.com/bobmatnyc/trusty-tools/issues/3072';

  type Phase = 'connecting' | 'daemon-unreachable' | 'no-session' | 'active';

  let phase = $state<Phase>('connecting');
  let session = $state<SessionSummary | null>(null);
  let error = $state<string | null>(null);

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

    try {
      const res = await fetch(`${base}/sessions`, { signal });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const body = (await res.json()) as SessionListResponse;
      if (signal.aborted) return;

      const active = pickActiveSession(body.sessions);
      if (!active) {
        phase = 'no-session';
        session = null;
        error = null;
        return;
      }
      session = active;
      phase = 'active';
      error = null;
    } catch (e) {
      if (!signal.aborted) {
        phase = 'daemon-unreachable';
        session = null;
        error = e instanceof Error ? e.message : String(e);
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
          <tr>
            <td colspan="6" class="pt-2 text-trusty-text/40">
              no REST snapshot route yet — see the gap notice below
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <p
      class="mt-3 text-[11px] text-trusty-text/30"
      title={`DOC-39 §4.7 AC-7.2's search/recall audit rows (lane, query, hit count, latency, requesting agent, age) are not renderable here — Event::SearchPerformed/Event::MemoryRecalled are SSE-only with no REST snapshot route and no persisted per-session history. Tracked at ${SEARCH_AUDIT_GAP_ISSUE}`}
    >
      search audit trail (AC-7.2): not yet implemented — see issue #3072
    </p>
  {/if}
</section>
