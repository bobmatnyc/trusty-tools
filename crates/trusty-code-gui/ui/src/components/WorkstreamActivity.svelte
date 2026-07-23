<script lang="ts">
  // Why: issue #3384 Scope B — Bob, after test-driving PR #3375's
  // workstream-first flow: "'SESSION MONITOR — no active session to
  // monitor' — this doesn't make sense in the workstream context." This
  // REFRAMES `SessionMonitor.svelte` around the ACTIVE WORKSTREAM rather
  // than "whichever session happens to be running anywhere in the daemon"
  // (the old `pickActiveSession(GET /sessions)` heuristic, with no
  // workstream awareness at all — a daemon with an active workstream AND a
  // stray projectless session from a prior run could show the WRONG one).
  // Renamed `SessionMonitor.svelte` -> `WorkstreamActivity.svelte` to match.
  //
  // **Issue #3446 turns this into the pane's own full chat stream.** Bob:
  // "Refactor the main chat pane similar to a TUI. Chat at the bottom,
  // enter as a button to the right (or enter), and the stream builds up."
  // This component now FILLS `WorkstreamTab.svelte`'s pane (that host sets
  // `h-full flex flex-col`; this component's own root is `flex h-full
  // min-h-0 flex-col`) rather than being a small bounded card, with
  // `StartWorkingForm` docked below it as a persistent input bar. The turn
  // list (`chatEntries`, via `lib/transcript.ts::selectChatEntries` — see
  // that module's own doc for why the OLD bounded/truncated
  // `selectTranscriptTail` was replaced outright, not kept alongside) is
  // rendered FULL, oldest-first, in a dedicated scrolling region so new
  // turns visually "build up" toward the bottom, TUI-style. An
  // auto-scroll-with-lock behavior (`userScrolledUp`, `onStreamScroll`)
  // keeps the view pinned to the newest turn on every poll UNLESS the
  // operator has manually scrolled up to read backlog — scrolling up never
  // gets yanked back down mid-read.
  //
  // **Empty state wording is verbatim from the issue**: "no active
  // workstream — pick a project to start", pointing at the primary flow
  // (`StartWorkingForm`, now docked directly below this pane in
  // `WorkstreamTab.svelte` — no separate navigation affordance needed, it's
  // already on screen).
  //
  // **Live activity via the SSE aggregation route (#3343) + activity events
  // (#3354), as a LATENCY NUDGE, not the authoritative source** — same
  // "REST poll is authoritative, SSE narrows the gap between ticks"
  // architecture `WorkstreamSwitcher.svelte` already established for this
  // exact route. `GET /workstreams/{id}/events` fans out EVERY event
  // belonging to a session bound to the active workstream (not just
  // activity-specific ones — `crate::workstreams::sse` module docs: "any
  // other event falls through to membership" — SessionAdded and any
  // ToolStarted/ToolFinished/etc. all arrive), so treating ANY received
  // frame as "something happened, refresh now" is a correct, conservative
  // reading — this component does not need to parse or buffer individual
  // event payloads (the ORIGINAL `SessionMonitor.svelte` module doc already
  // reasoned through why becoming a client-side SSE event-log consumer would
  // be more state/derivation than a thin client should introduce ad hoc;
  // that reasoning is unchanged here, just applied to the workstream-level
  // stream instead of a session-level one).
  //
  // **Session selection is now scoped to the active workstream's own
  // membership** (`pickActiveSessionInWorkstream`, `lib/session-status.ts`)
  // — never a session bound to a different (or no) workstream. A workstream
  // with zero bound sessions (freshly minted, e.g. via the CLI/RPC directly
  // with no task yet) renders its OWN honest sub-empty-state ("no activity
  // yet") rather than falling through to a stray unrelated session.
  //
  // **Falls back to the just-minted, not-yet-active workstream when the
  // daemon reports no real active pointer** (code-critic PR #3392 review,
  // MEDIUM). `StartWorkingForm.svelte` mints a workstream and runs the
  // first task BEFORE activating it (deliberately — see that component's
  // own docs); if activation then fails, the daemon's real
  // `active_workstream_id` stays whatever it was before, and this pane used
  // to show "no active workstream — pick a project to start" WHILE the
  // operator's just-accepted task was actually running — a narrow repeat of
  // the exact complaint issue #3384 was filed over. `refresh()` now falls
  // back to `pending-workstream.svelte.ts`'s shared marker (see that
  // module's own docs) when the real pointer is absent, but ONLY when it
  // names a workstream that genuinely exists in the CURRENT `GET
  // /workstreams` response — never invented data.
  //
  // What: Two REST polls per tick, chained: `GET /workstreams` (this
  // component's OWN poll — every component that needs workstream state
  // polls independently, the established house convention) to find the
  // active workstream, then — only when one is active — `GET /sessions` +
  // `GET /sessions/{id}` + `GET /sessions/{id}/transcript` for whichever
  // session `pickActiveSessionInWorkstream` selects among its bound ids.
  // Same `$effect`/`AbortController`/`setInterval` shape every poller in
  // this codebase uses (one controller for the whole mounted lifetime,
  // `signal.aborted` re-checked after every `await` before any state
  // write). A second, independent `$effect` ticks a local `now` every
  // second purely for elapsed-time redisplay — no network call, own
  // teardown. A THIRD `$effect` opens an `EventSource` on
  // `/workstreams/{id}/events` whenever the active workstream's id changes,
  // mirroring `WorkstreamSwitcher.svelte`'s identical pattern; it no-ops
  // when `EventSource` is unavailable or no workstream is active. A FOURTH
  // `$effect` (issue #3446, new) auto-scrolls the stream container to its
  // newest entry whenever `chatEntries` changes, unless `userScrolledUp`.
  // Renders one of four phases (`connecting`/`daemon-unreachable`/
  // `no-workstream`/`ready`), same never-hidden discipline as `StatusBar`
  // (DOC-39 AC-4.2); `ready` itself branches on whether a bound session was
  // found. A `GET /sessions/{id}` 404 immediately after a successful list
  // (session vanished mid-poll) degrades to the session-level sub-empty-
  // state, not `daemon-unreachable` — the daemon answered fine. Cancel
  // requires a two-step in-card confirm (no `window.confirm()`) and forces
  // an immediate `refresh()` on success rather than waiting for the next
  // poll tick — all carried over unchanged from `SessionMonitor.svelte`.
  //
  // **Live delta streaming (tcode streaming epic #3696, Slice 3), gated
  // behind `SSE_ENABLED` (`VITE_TCODE_SSE`, default OFF).** A FIFTH `$effect`
  // opens a SESSION-scoped `EventSource` on `GET /sessions/{id}/events` (a
  // different route than the workstream-level nudge stream above — this one
  // carries full event payloads, not just an "arrived" ping) whenever the
  // bound session's id changes, folding every `agent_message_delta` envelope
  // through `lib/transcript.ts::applyDelta` into `streamBubbles`. The poll
  // (`chatEntries`, still the authoritative baseline — unchanged from before)
  // and the live stream (`liveEntries`, rendered after `chatEntries`) are
  // reconciled by a SIXTH `$effect`: once a fresh poll transcript lands, any
  // bubble already marked `done` is dropped (the poll now has it as real
  // history), while an in-progress bubble survives poll ticks until it
  // finishes. `SSE_ENABLED` OFF (the default) short-circuits both new effects
  // to a no-op, leaving every pre-Slice-3 behavior byte-for-byte unchanged.
  //
  // Test: `lib/session-status.test.ts::pickActiveSessionInWorkstream`
  // covers the scoping rule; `lib/pending-workstream.test.ts` covers the
  // fallback marker's own read/write contract; `lib/transcript.test.ts`
  // covers the pure chat-entry/elapsed helpers AND `applyDelta`'s tuple-keyed
  // reducer (Slice 3); `WorkstreamActivity.test.ts` covers the four phases
  // (incl. the bound-session-vs-empty-workstream sub-states and the
  // pending-workstream fallback), the SSE-subscription reactivity fix
  // (code-critic PR #3392 review, HIGH), the auto-scroll-with-lock behavior
  // (issue #3446), cancel, and (Slice 3) the gated delta-stream rendering
  // incremental text before `done` and finalizing the bubble; `App.test.ts`
  // pins that this component mounts inside `.body`.
  import { resolveActiveWorkstreamId, setActiveWorkstreamId } from '../lib/active-workstream.svelte';
  import { apiBase } from '../lib/api-config';
  import {
    pickActiveSessionInWorkstream,
    TERMINAL_SESSION_STATUSES,
    type SessionDetail,
    type SessionListResponse,
  } from '../lib/session-status';
  import {
    applyDelta,
    formatElapsed,
    selectChatEntries,
    transcriptFilename,
    type AgentMessageDeltaEvent,
    type StreamBubble,
    type TranscriptRecord,
  } from '../lib/transcript';
  import { fetchWorkstreams, workstreamLabel, type Workstream } from '../lib/workstreams';
  import { untrack } from 'svelte';

  const POLL_MS = 5000; // matches StatusBar.svelte's poll cadence
  const TICK_MS = 1000; // local redisplay tick only — no network call

  /** Feature gate for the Slice 3 session-level delta SSE stream (tcode
   * streaming epic #3696). Default OFF — ships dark, flipped on later by
   * setting `VITE_TCODE_SSE=true` at build/dev time — so today's poll-only
   * behavior is fully unchanged until this is explicitly enabled. See the
   * module doc's SSE section below for what turning it on adds. */
  const SSE_ENABLED = import.meta.env.VITE_TCODE_SSE === 'true';

  /** How close to the bottom (in px) still counts as "at the bottom" for
   * auto-scroll purposes — a small fudge factor, not an exact 0, so a
   * sub-pixel layout rounding difference doesn't spuriously flip
   * `userScrolledUp` to true every tick. */
  const SCROLL_LOCK_THRESHOLD_PX = 48;

  /** Tracks the future AC-6.3 gap (see the Why block above). */
  const SEARCH_RECALL_GAP_ISSUE = 'https://github.com/bobmatnyc/trusty-tools/issues/3108';

  type Phase = 'connecting' | 'daemon-unreachable' | 'no-workstream' | 'ready';
  type CancelPhase = 'idle' | 'confirming' | 'cancelling';

  let phase = $state<Phase>('connecting');
  let activeWorkstream = $state<Workstream | null>(null);
  let session = $state<SessionDetail | null>(null);
  let transcript = $state<TranscriptRecord | null>(null);
  let error = $state<string | null>(null);
  let cancelPhase = $state<CancelPhase>('idle');
  let now = $state(Date.now());

  // Live delta bubbles (Slice 3, gated behind `SSE_ENABLED`) — see the
  // dedicated `$effect` below for how these are populated/reset/reconciled.
  let streamBubbles = $state<StreamBubble[]>([]);

  let pollController: AbortController | null = null;

  // Auto-scroll-with-lock (issue #3446) — see the module doc above.
  // `$state` here isn't for reactivity (nothing derives from `streamEl`
  // itself) — it silences Svelte 5's non_reactive_update warning for a
  // `bind:this` target reassigned after mount.
  let streamEl = $state<HTMLDivElement | undefined>(undefined);
  let userScrolledUp = $state(false);

  function onStreamScroll() {
    if (!streamEl) return;
    const distanceFromBottom = streamEl.scrollHeight - streamEl.scrollTop - streamEl.clientHeight;
    userScrolledUp = distanceFromBottom > SCROLL_LOCK_THRESHOLD_PX;
  }

  async function refresh(signal: AbortSignal) {
    let base: string;
    try {
      base = await apiBase();
    } catch (e) {
      if (!signal.aborted) {
        phase = 'daemon-unreachable';
        activeWorkstream = null;
        session = null;
        transcript = null;
        error = e instanceof Error ? e.message : String(e);
      }
      return;
    }
    if (signal.aborted) return;

    let list;
    try {
      list = await fetchWorkstreams(base, signal);
    } catch (e) {
      if (!signal.aborted) {
        phase = 'daemon-unreachable';
        activeWorkstream = null;
        session = null;
        transcript = null;
        error = e instanceof Error ? e.message : String(e);
      }
      return;
    }
    if (signal.aborted) return;

    // Fallback to the just-minted, not-yet-active workstream when the
    // daemon reports no real active pointer (code-critic PR #3392 review,
    // MEDIUM — see `lib/pending-workstream.svelte.ts`'s own docs for why:
    // an activation failure after a successful task-run must not make this
    // card claim "no active workstream" while that task is actually
    // running). The resolution rule (real pointer first, pending marker
    // only when it names a listed workstream) now lives in
    // `lib/active-workstream.svelte.ts::resolveActiveWorkstreamId` so this
    // poller and `WorkstreamSwitcher.svelte`'s apply the IDENTICAL rule
    // before publishing to the shared active-workstream store (code-critic
    // PR #3460 review, HIGH 2 — `StartWorkingForm` watches that store to
    // re-target chat continuation on any active-workstream change).
    const resolvedId = resolveActiveWorkstreamId(list);
    setActiveWorkstreamId(resolvedId);
    const active = list.workstreams.find((w) => w.id === resolvedId) ?? null;
    if (!active) {
      phase = 'no-workstream';
      activeWorkstream = null;
      session = null;
      transcript = null;
      error = null;
      cancelPhase = 'idle';
      userScrolledUp = false;
      return;
    }
    if (activeWorkstream?.id !== active.id) {
      // The active workstream changed under us — drop any pending confirm
      // rather than let it apply to a session under a different workstream,
      // and release the scroll lock (code-critic PR #3460 review, MEDIUM 3):
      // a "scrolled up to read backlog" position in the OLD workstream's
      // stream is meaningless in the new one and would otherwise leave the
      // fresh stream stuck mid-scroll instead of pinned to its newest turn.
      cancelPhase = 'idle';
      userScrolledUp = false;
    }
    activeWorkstream = active;
    phase = 'ready';
    error = null;

    let sessions: SessionListResponse['sessions'];
    try {
      const res = await fetch(`${base}/sessions`, { signal });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const body = (await res.json()) as SessionListResponse;
      sessions = body.sessions;
    } catch (e) {
      // The workstream itself is confirmed active (we just listed it) — a
      // sessions-fetch failure here is transient/partial, not "no daemon".
      if (!signal.aborted) {
        session = null;
        transcript = null;
        error = e instanceof Error ? e.message : String(e);
      }
      return;
    }
    if (signal.aborted) return;

    const boundSession = pickActiveSessionInWorkstream(sessions, active.session_ids);
    if (!boundSession) {
      // A real, active workstream with nothing bound yet — its own honest
      // sub-empty-state, never a fallback to an unrelated session.
      session = null;
      transcript = null;
      error = null;
      cancelPhase = 'idle';
      userScrolledUp = false;
      return;
    }
    if (session?.id !== boundSession.id) {
      // Same rule as the workstream-change reset above (code-critic PR
      // #3460 review, MEDIUM 3): a scroll-lock captured against one
      // session's stream must not survive into a different session's.
      cancelPhase = 'idle';
      userScrolledUp = false;
    }

    try {
      const res = await fetch(`${base}/sessions/${boundSession.id}`, { signal });
      if (res.status === 404) {
        // Reachable daemon, just-listed session vanished mid-poll.
        if (!signal.aborted) {
          session = null;
          transcript = null;
          error = null;
          cancelPhase = 'idle';
          userScrolledUp = false;
        }
        return;
      }
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const detail = (await res.json()) as SessionDetail;
      if (signal.aborted) return;
      session = detail;
      error = null;
    } catch (e) {
      if (!signal.aborted) {
        error = e instanceof Error ? e.message : String(e);
      }
      return;
    }
    if (signal.aborted) return;

    try {
      const res = await fetch(`${base}/sessions/${boundSession.id}/transcript`, { signal });
      if (res.status === 404) {
        if (!signal.aborted) {
          session = null;
          transcript = null;
          error = null;
          cancelPhase = 'idle';
          userScrolledUp = false;
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
    // One controller for the whole mounted lifetime — same shape as every
    // other poller in this codebase.
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
    // "elapsed" advances smoothly without waiting on POLL_MS.
    const timer = setInterval(() => {
      now = Date.now();
    }, TICK_MS);
    return () => clearInterval(timer);
  });

  // Re-subscribes ONLY when the ACTIVE id itself changes (code-critic PR
  // #3392 review, HIGH — Svelte 5 tracks reference equality of the `$state`
  // object read inside the effect, not deep-equality of its fields; since
  // `refresh()` reassigns `activeWorkstream` to a FRESHLY-PARSED object on
  // every poll tick even when the active id is unchanged, an effect that
  // reads `activeWorkstream?.id` directly re-runs (close + reopen the
  // `EventSource`) on every tick — ~360 reconnects/active-half-hour,
  // dropping any event landing in the close->reopen gap, and needlessly
  // loading the daemon. `activeWorkstreamId` below is a `$derived` PRIMITIVE
  // — Svelte 5 suppresses a dependent's re-run when a `$derived` recomputes
  // to the SAME primitive value, so the effect only re-runs when the id
  // itself actually changes. `WorkstreamSwitcher.svelte` carries the
  // identical fix for the identical pre-existing bug (issue #3356) in the
  // same review pass.
  let activeWorkstreamId = $derived(activeWorkstream?.id ?? null);

  // Live-activity nudge (issue #3384), mirroring `WorkstreamSwitcher.svelte`'s
  // identical SSE pattern over the SAME route. Any received frame (see the
  // Why block above for why this component does not need to inspect
  // payloads) triggers an immediate `refresh()` — a latency optimization
  // over the POLL_MS cadence, never the authoritative source; it no-ops
  // (falls back to polling alone) when `EventSource` is unavailable or no
  // workstream is active.
  $effect(() => {
    const id = activeWorkstreamId;
    if (!id || typeof EventSource === 'undefined') return;

    let source: EventSource | null = null;
    let cancelled = false;

    void (async () => {
      let base: string;
      try {
        base = await apiBase();
      } catch {
        return;
      }
      if (cancelled) return;
      source = new EventSource(`${base}/workstreams/${id}/events`);
      source.onmessage = () => {
        if (pollController) void refresh(pollController.signal);
      };
    })();

    return () => {
      cancelled = true;
      source?.close();
    };
  });

  // Same fresh-object-every-poll-tick problem `activeWorkstreamId` solves for
  // the workstream-level effect above applies here too — `session` is
  // reassigned to a newly-parsed object on every `POLL_MS` tick even when the
  // bound session itself hasn't changed, so this effect keys off a `$derived`
  // PRIMITIVE rather than reading `session?.id` directly.
  let sessionId = $derived(session?.id ?? null);

  // Live delta stream (tcode streaming epic #3696, Slice 3) — gated behind
  // `SSE_ENABLED` (default OFF, see that const's doc). When enabled, opens an
  // `EventSource` on the SESSION-scoped `GET /sessions/{id}/events` route
  // (distinct from the WORKSTREAM-scoped nudge route above) and folds every
  // `agent_message_delta` envelope through `applyDelta` so in-progress turns
  // render incrementally rather than waiting for the next `POLL_MS` transcript
  // poll. Re-subscribes only when `sessionId` itself changes (same reasoning
  // as the effect above), resetting `streamBubbles` to `[]` on every
  // (re)subscribe — a bubble from a previous session must never bleed into
  // the next one's stream. `GET /sessions/{id}/events` replays that session's
  // full ring buffer before chaining live events (see `crate::serve::http`'s
  // `session_events_sse` docs), so a fresh subscribe naturally rebuilds any
  // turns that were mid-stream at connect time; every frame carries its
  // envelope `seq`, forwarded into `AgentMessageDeltaEvent.seq` so `applyDelta`
  // can order/dedup bubbles across a replay-then-live boundary. Only
  // `agent_message_delta` envelopes are handled — every other `kind` is
  // ignored here (the workstream-level effect above already treats any frame
  // as a generic "refresh now" nudge).
  $effect(() => {
    const id = sessionId;
    streamBubbles = [];
    if (!SSE_ENABLED || !id || typeof EventSource === 'undefined') return;

    let source: EventSource | null = null;
    let cancelled = false;

    void (async () => {
      let base: string;
      try {
        base = await apiBase();
      } catch {
        return;
      }
      if (cancelled) return;
      source = new EventSource(`${base}/sessions/${id}/events`);
      source.onmessage = (e: MessageEvent) => {
        if (cancelled) return;
        let envelope: { seq?: unknown; event?: { type?: unknown; [key: string]: unknown } };
        try {
          envelope = JSON.parse(e.data);
        } catch {
          return;
        }
        const event = envelope.event;
        if (
          typeof envelope.seq !== 'number' ||
          !event ||
          event.type !== 'agent_message_delta' ||
          typeof event.agent !== 'string' ||
          typeof event.agent_id !== 'string' ||
          typeof event.turn_id !== 'string' ||
          typeof event.delta !== 'string' ||
          typeof event.done !== 'boolean'
        ) {
          return;
        }
        const delta: AgentMessageDeltaEvent = {
          session_id: id,
          agent: event.agent,
          agent_id: event.agent_id,
          turn_id: event.turn_id,
          delta: event.delta,
          done: event.done,
          seq: envelope.seq,
        };
        streamBubbles = applyDelta(streamBubbles, delta);
      };
    })();

    return () => {
      cancelled = true;
      source?.close();
    };
  });

  // Poll-driven reconcile (Slice 3): once a FRESH poll transcript lands, the
  // polled `chatEntries` are the durable record from here for anything the
  // stream had already marked `done` — drop those bubbles so a finished turn
  // is never shown twice (once as a live bubble, again once the poll catches
  // up). An in-progress (`!done`) bubble is deliberately left alone across
  // poll ticks — the poll cannot yet have observed it as a finished turn.
  // Reads `streamBubbles` via `untrack` so this effect's dependency is
  // `transcript` alone (assigning `streamBubbles` here must not make the
  // effect re-run itself).
  $effect(() => {
    void transcript;
    const current = untrack(() => streamBubbles);
    if (current.some((b) => b.done)) {
      streamBubbles = current.filter((b) => !b.done);
    }
  });

  let chatEntries = $derived(transcript ? selectChatEntries(transcript.turns) : []);
  /** Live streaming entries rendered after `chatEntries` — see the Slice 3
   * effects above for how `streamBubbles` is populated/reconciled. Carries
   * `streaming` so the template can apply the in-progress affordance only to
   * a bubble still receiving deltas. */
  let liveEntries = $derived(
    streamBubbles.map((b) => ({ agent: b.agent, text: b.text, streaming: !b.done })),
  );
  let canCancel = $derived(session !== null && !TERMINAL_SESSION_STATUSES.has(session.status));

  // Download-transcript affordance (issue #3526). Shown once a bound session
  // is selected for the active workstream — that session's transcript is what
  // the daemon renders to Markdown. The DAEMON is the single source of truth
  // for the Markdown format (`GET /sessions/{id}/transcript.md`,
  // `crate::serve::rest::sessions::render_transcript_markdown`), so the same
  // bytes a developer gets via `curl` in local dev are what this button saves
  // — no second, drift-prone TS serializer. See that endpoint's docs.
  let canDownload = $derived(activeWorkstream !== null && session !== null);

  /**
   * Fetch the daemon-rendered Markdown transcript for the active workstream's
   * session and trigger a browser download of it. The serialization happens
   * server-side (single source of truth — see `canDownload`'s note); this is
   * purely the DOM-side save (Blob + object URL + a synthetic `<a download>`
   * click), so it lives in the component rather than `lib/transcript.ts`
   * (`transcriptFilename` — the timestamped filename — stays pure there).
   * No-ops when there is nothing to download or the runtime lacks the
   * Blob/URL APIs (SSR/older environments); a fetch failure surfaces in the
   * existing in-pane `error` line rather than throwing.
   */
  async function downloadTranscript() {
    if (!session || !activeWorkstream) return;
    if (typeof URL === 'undefined' || typeof URL.createObjectURL !== 'function') return;

    let markdown: string;
    try {
      const base = await apiBase();
      const res = await fetch(`${base}/sessions/${session.id}/transcript.md`);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      markdown = await res.text();
    } catch (e) {
      error = `transcript download failed — ${e instanceof Error ? e.message : String(e)}`;
      return;
    }

    const filename = transcriptFilename(activeWorkstream.id, Date.now());
    const blob = new Blob([markdown], { type: 'text/markdown;charset=utf-8' });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = filename;
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
    URL.revokeObjectURL(url);
  }

  $effect(() => {
    // Auto-scroll to the newest entry as the stream "builds up" (issue
    // #3446), unless the operator has scrolled up to read backlog. Reads
    // `chatEntries` (not `transcript`) so this only fires when the parsed
    // turn list itself actually changes shape/content, not on every raw
    // poll tick that happens to return byte-identical turns. Also reads
    // `liveEntries` (Slice 3) so growing streamed text keeps auto-scrolling
    // too, not just a completed-turn poll refresh.
    void chatEntries;
    void liveEntries;
    if (streamEl && !userScrolledUp) {
      streamEl.scrollTop = streamEl.scrollHeight;
    }
  });

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

<section class="flex h-full min-h-0 flex-col">
  {#if phase === 'connecting'}
    <div class="flex flex-1 items-center justify-center">
      <p class="text-xs text-trusty-text-muted">connecting…</p>
    </div>
  {:else if phase === 'daemon-unreachable'}
    <div class="flex flex-1 items-center justify-center">
      <p class="flex items-center gap-1.5 text-xs text-status-error">
        <span class="h-1.5 w-1.5 rounded-full bg-status-error"></span>
        daemon unreachable{error ? ` — ${error}` : ''}
      </p>
    </div>
  {:else if phase === 'no-workstream'}
    <div class="flex flex-1 items-center justify-center">
      <p class="flex items-center gap-1.5 text-xs text-trusty-text-muted">
        <span class="h-1.5 w-1.5 rounded-full bg-status-neutral"></span>
        no active workstream — pick a project to start
      </p>
    </div>
  {:else if activeWorkstream}
    <div
      class="flex shrink-0 items-center justify-between gap-2 border-b border-trusty-border bg-trusty-raised px-4 py-2"
    >
      <span class="font-mono text-[11px] uppercase tracking-wide text-trusty-text-secondary">
        {workstreamLabel(activeWorkstream)}
      </span>
      <div class="flex items-center gap-3">
        {#if session}
          <span class="flex items-center gap-2 font-mono text-[11px]">
            <span class={`h-1.5 w-1.5 rounded-full ${statusDotClass(session.status)}`}></span>
            <span class="font-semibold uppercase tracking-wide text-trusty-text">{session.status}</span>
            <span class="text-trusty-text-muted">·</span>
            <span class="text-trusty-text-secondary">{formatElapsed(session.created_at, now)}</span>
          </span>
        {/if}
        {#if canDownload}
          <button
            type="button"
            class="rounded-sm border-1.5 border-trusty-border-strong bg-trusty-raised px-2.5 py-1 font-mono text-[11px] font-semibold uppercase tracking-wide text-trusty-text-secondary hover:border-trusty-primary hover:text-trusty-primary"
            title="Download this workstream's full transcript as a Markdown file"
            onclick={downloadTranscript}
          >
            download transcript
          </button>
        {/if}
      </div>
    </div>

    <div
      bind:this={streamEl}
      onscroll={onStreamScroll}
      class="activity-stream min-h-0 flex-1 space-y-3 overflow-y-auto p-4"
    >
      {#if !session}
        <p class="flex items-center gap-1.5 text-xs text-trusty-text-muted">
          <span class="h-1.5 w-1.5 rounded-full bg-status-neutral"></span>
          no activity yet
        </p>
      {:else}
        <p class="truncate text-sm font-semibold text-trusty-text" title={session.task}>
          {session.task}
        </p>

        {#if chatEntries.length === 0 && liveEntries.length === 0}
          <p class="text-xs text-trusty-text-muted">no turns recorded yet</p>
        {:else}
          {#each chatEntries as entry, i (i)}
            <div class="text-xs">
              <span class="font-mono font-semibold uppercase tracking-wide text-trusty-text"
                >{entry.agent}</span
              >
              <p class="mt-0.5 whitespace-pre-wrap text-trusty-text-secondary">{entry.text}</p>
            </div>
          {/each}
          {#each liveEntries as entry, i (i)}
            <div class="text-xs">
              <span class="inline-flex items-center gap-1.5 font-mono font-semibold uppercase tracking-wide text-trusty-text">
                {entry.agent}
                {#if entry.streaming}
                  <span
                    class="h-1.5 w-1.5 animate-pulse rounded-full bg-status-warn"
                    title="streaming…"
                  ></span>
                {/if}
              </span>
              <p class="mt-0.5 whitespace-pre-wrap text-trusty-text-secondary">{entry.text}</p>
            </div>
          {/each}
        {/if}

        {#if error}
          <p class="text-xs text-status-error">{error}</p>
        {/if}
      {/if}
    </div>

    {#if session}
      <div class="flex shrink-0 items-center gap-2 border-t border-trusty-border px-4 py-2">
        {#if canCancel}
          {#if cancelPhase === 'idle'}
            <button
              type="button"
              class="rounded-sm border-1.5 border-trusty-border-strong bg-trusty-raised px-2.5 py-1 font-mono text-[11px] font-semibold uppercase tracking-wide text-trusty-text-secondary hover:border-trusty-primary hover:text-trusty-primary"
              onclick={() => (cancelPhase = 'confirming')}
            >
              cancel
            </button>
          {:else if cancelPhase === 'confirming'}
            <span class="font-mono text-[11px] uppercase tracking-wide text-trusty-text-muted"
              >confirm cancel?</span
            >
            <button
              type="button"
              class="rounded-sm border-1.5 border-trusty-primary bg-trusty-primary/10 px-2.5 py-1 font-mono text-[11px] font-semibold uppercase tracking-wide text-trusty-primary-hover hover:bg-trusty-primary hover:text-trusty-text-inverse"
              onclick={doCancel}
            >
              confirm cancel
            </button>
            <button
              type="button"
              class="rounded-sm border-1.5 border-trusty-border-strong bg-trusty-card px-2.5 py-1 font-mono text-[11px] font-semibold uppercase tracking-wide text-trusty-text-secondary hover:border-trusty-primary hover:text-trusty-primary"
              onclick={() => (cancelPhase = 'idle')}
            >
              never mind
            </button>
          {:else}
            <span class="font-mono text-[11px] uppercase tracking-wide text-trusty-text-muted"
              >cancelling…</span
            >
          {/if}
        {/if}
      </div>
    {/if}

    <p
      class="shrink-0 border-t border-trusty-border px-4 py-1 text-[11px] text-trusty-text-muted"
      title={`DOC-39 §4.6 AC-6.3's live search/memory-recall monitor (lane/query/hit_count/latency, docked-rail → inline settle) is not implemented here — GET /sessions/{id}/search-audit now exists (issue #3072) but this card has not been wired to it yet. Tracked at ${SEARCH_RECALL_GAP_ISSUE}`}
    >
      search/recall monitor (AC-6.3): not yet implemented — see issue #3108
    </p>
  {/if}
</section>
