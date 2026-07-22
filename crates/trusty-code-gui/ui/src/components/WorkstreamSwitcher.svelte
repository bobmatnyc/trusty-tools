<script lang="ts">
  // Why: DOC-39 §2/§8 reserved this exact header slot (issue #3153 shell
  // rebuild, PR #3301) — `AppHeader.svelte` shipped a read-only label plus a
  // disabled `▾` placeholder because "Phase 1 has no workstream registry
  // yet". DOC-48 (epic #3292) built that registry (domain model,
  // persistence, activation-lock exclusivity — `crates/trusty-code/src/
  // workstreams/`) and its Phase C explicitly scopes this component: state
  // display, list view with an active indicator, activation (switch), close
  // (confirm first), and rename — DOC-48 §8 "GUI workstream switcher"
  // (issue #3300). This is that component filling the reserved slot;
  // `AppHeader.svelte` itself is otherwise untouched.
  //
  // What: Polls `GET /workstreams` every `POLL_MS` (the same
  // `$effect`/`AbortController`/`setInterval` shape `StatusBar.svelte`/
  // `WorkstreamActivity.svelte` already establish — one controller for the
  // whole mounted lifetime, `signal.aborted` re-checked after every
  // `await`). A second `$effect` opens an `EventSource` on
  // `/workstreams/{active_id}/events` whenever the polled
  // `active_workstream_id` changes (DOC-48 §5.3) and calls `refresh()`
  // immediately on any `workstream_activation_changed`/
  // `workstream_state_inferred` frame (`lib/workstreams.ts::isActivationSignal`)
  // for a low-latency update between poll ticks; it no-ops (falls back to
  // the `POLL_MS` cadence alone) when `EventSource` is unavailable or no
  // workstream is active — there is no daemon-wide "any workstream changed"
  // push channel today (a real API gap: DOC-39 §2.1 C-2 — per-workstream SSE
  // requires already knowing which workstream to subscribe to), so polling
  // stays the AUTHORITATIVE source and SSE is purely a latency optimization.
  // Every `refresh()` call is guarded by a monotonic sequence number
  // (`requestSeq`, code-critic PR #3356 review MEDIUM 1) — the poll interval
  // and an SSE-triggered refresh share one `AbortController`, so neither
  // aborts the other, and a slow earlier call resolving AFTER a fresher
  // later one must not clobber it; only the call that is still the MOST
  // RECENTLY STARTED one by the time it resolves is allowed to commit.
  //
  // Renders a trigger button (state dot + name, `▾`) that opens a dropdown
  // panel listing every workstream with an active indicator. Clicking a
  // non-active row activates it (`workstream.activate{force: false}`,
  // DOC-48 §6.1); a `409` (`ActiveConflictError` — someone else activated
  // concurrently) shows an inline conflict banner with a Refresh action
  // rather than silently retrying with `force: true` (an explicit switch is
  // never silent, and DOC-48's own wording for this issue's scope is
  // "surface it, offer refresh"). Rename swaps a row into an inline
  // text-input edit (no `window.prompt()` — see `WorkstreamActivity.svelte`'s
  // identical rationale); allowed on a closed row too, matching the
  // backend's `rename_closed_workstream_succeeds` contract (§4.4: closure
  // only rejects new session bindings, never label edits). Close requires
  // the same two-step in-row confirm `WorkstreamActivity.svelte`'s cancel action
  // already establishes (no `window.confirm()`). `resetRowState()` clears
  // every armed rename/close/conflict control at EVERY open-state
  // transition (code-critic PR #3356 review, HIGH) — both `toggleOpen()`
  // directions, the backdrop-dismiss button, and a successful activation —
  // so an armed control can never survive a dismiss-and-reopen cycle.
  //
  // Test: `WorkstreamSwitcher.test.ts` covers every phase/interaction, the
  // backdrop-dismiss row-state reset, and the out-of-order refresh guard;
  // `lib/workstreams.test.ts` covers the network/pure-logic layer this
  // component calls into; `App.test.ts` pins that `AppHeader` (and by
  // extension this component) mounts inside `.hdr`.
  import { resolveActiveWorkstreamId, setActiveWorkstreamId } from '../lib/active-workstream.svelte';
  import { apiBase } from '../lib/api-config';
  import {
    ActiveConflictError,
    activateWorkstream,
    closeWorkstream,
    fetchWorkstreams,
    isActivationSignal,
    renameWorkstream,
    workstreamLabel,
    workstreamStateDotClass,
    type Workstream,
    type WorkstreamEventEnvelope,
    type WorkstreamListResponse,
  } from '../lib/workstreams';

  const POLL_MS = 5000; // matches StatusBar.svelte's poll cadence

  type Phase = 'connecting' | 'daemon-unreachable' | 'ready';

  let phase = $state<Phase>('connecting');
  let list = $state<WorkstreamListResponse | null>(null);
  let error = $state<string | null>(null);
  let open = $state(false);
  let busyId = $state<string | null>(null);
  let conflictMessage = $state<string | null>(null);
  let renameId = $state<string | null>(null);
  let renameDraft = $state('');
  let renameBusy = $state(false);
  let closeConfirmId = $state<string | null>(null);
  let closeBusy = $state(false);

  let pollController: AbortController | null = null;

  // Monotonic guard against out-of-order `refresh()` resolution (code-critic
  // PR #3356 review, MEDIUM 1): the poll interval and the SSE-triggered
  // refresh share one `AbortController`/signal, so neither call aborts the
  // other — a slow earlier fetch (e.g. an interval tick) can resolve AFTER a
  // later, fresher one (e.g. an SSE-triggered refresh right after a mutation)
  // and clobber it with stale data. Every `refresh()` call stamps its own
  // sequence number and only commits `list`/`phase`/`error` if it is still
  // the MOST RECENTLY STARTED call by the time it resolves — an in-flight
  // call that a newer one has since superseded silently drops its result
  // instead of overwriting fresher state (the newer call already committed,
  // or will).
  let requestSeq = 0;

  function msg(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  async function refresh(signal: AbortSignal) {
    const seq = ++requestSeq;
    const stale = () => signal.aborted || seq !== requestSeq;

    let base: string;
    try {
      base = await apiBase();
    } catch (e) {
      if (!stale()) {
        phase = 'daemon-unreachable';
        list = null;
        error = msg(e);
      }
      return;
    }
    if (stale()) return;

    try {
      const body = await fetchWorkstreams(base, signal);
      if (stale()) return;
      list = body;
      phase = 'ready';
      error = null;
      // Publish the resolved active id to the shared cross-module store
      // (code-critic PR #3460 review, HIGH 2) so `StartWorkingForm`'s chat
      // continuation re-targets when the active workstream changes via
      // THIS switcher — which never touches the selected-project store.
      // MUST use the same resolution rule `WorkstreamActivity.svelte`'s
      // poller applies (see `lib/active-workstream.svelte.ts`'s module doc
      // for why divergent rules would flip-flop the shared value).
      setActiveWorkstreamId(resolveActiveWorkstreamId(body));
    } catch (e) {
      if (!stale()) {
        phase = 'daemon-unreachable';
        list = null;
        error = msg(e);
      }
    }
  }

  $effect(() => {
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

  // Re-subscribes ONLY when the ACTIVE id itself changes (code-critic PR
  // #3392 review, HIGH — carried-in fix for a pre-existing bug from issue
  // #3300/PR #3356, identified and fixed alongside the identical pattern in
  // `WorkstreamActivity.svelte`, issue #3384). `refresh()` reassigns `list`
  // to a FRESHLY-PARSED object on every poll tick — Svelte 5 invalidates on
  // reference inequality of the `$state` source itself, so an effect
  // reading `list?.active_workstream_id` directly re-ran (closing +
  // reopening the `EventSource`) on EVERY poll tick, not only when the
  // active id actually changed: ~360 reconnects/active-half-hour, dropping
  // any event landing in the close->reopen gap, and needlessly loading the
  // daemon — directly contradicting this comment's own original claim.
  // `activeWorkstreamId` below is a `$derived` PRIMITIVE; Svelte 5
  // suppresses a dependent's re-run when a `$derived` recomputes to the
  // SAME primitive value, so the effect now only re-runs when the id
  // itself actually changes.
  let activeWorkstreamId = $derived(list?.active_workstream_id ?? null);

  $effect(() => {
    const activeId = activeWorkstreamId;
    if (!activeId || typeof EventSource === 'undefined') return;

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
      source = new EventSource(`${base}/workstreams/${activeId}/events`);
      source.onmessage = (e: MessageEvent<string>) => {
        try {
          const envelope = JSON.parse(e.data) as WorkstreamEventEnvelope;
          if (isActivationSignal(envelope) && pollController) {
            void refresh(pollController.signal);
          }
        } catch {
          // Ignore a malformed SSE frame — the next poll tick self-heals.
        }
      };
    })();

    return () => {
      cancelled = true;
      source?.close();
    };
  });

  let activeWorkstream = $derived(
    list?.workstreams.find((w) => w.id === list?.active_workstream_id) ?? null,
  );

  function triggerLabel(): string {
    if (phase === 'connecting') return 'connecting…';
    if (phase === 'daemon-unreachable') return 'daemon unreachable';
    if (!list || list.workstreams.length === 0) return 'no workstreams yet';
    if (activeWorkstream) return workstreamLabel(activeWorkstream);
    return 'no active workstream';
  }

  function triggerDotClass(): string {
    if (phase !== 'ready') return 'bg-status-error';
    if (activeWorkstream) return workstreamStateDotClass(activeWorkstream.state);
    return 'bg-status-neutral';
  }

  // Clears every armed per-row destructive/edit control (code-critic PR
  // #3356 review, HIGH): rename-in-progress, close-confirm-armed, and any
  // conflict banner. Called at EVERY open-state transition (both directions
  // of `toggleOpen()`, the backdrop-dismiss button, and the post-activation
  // success path in `onActivateClick`) — not just "closing via the trigger"
  // — so a row armed via `closeConfirmId`/`renameId` can never survive a
  // dismiss-and-reopen cycle (repro: open -> arm close on row B ->
  // backdrop-dismiss -> reopen used to still show row B pre-armed).
  function resetRowState() {
    renameId = null;
    renameDraft = '';
    closeConfirmId = null;
    conflictMessage = null;
  }

  function toggleOpen() {
    if (phase !== 'ready' || !list || list.workstreams.length === 0) return;
    open = !open;
    resetRowState();
  }

  function closePanel() {
    open = false;
    resetRowState();
  }

  async function onActivateClick(w: Workstream) {
    if (w.state === 'active' || w.state === 'closed' || busyId) return;
    busyId = w.id;
    conflictMessage = null;
    try {
      const base = await apiBase();
      await activateWorkstream(base, w.id);
      closePanel();
      if (pollController) await refresh(pollController.signal);
    } catch (e) {
      if (e instanceof ActiveConflictError) {
        conflictMessage = 'another workstream was just activated — refresh to see the current one';
      } else {
        error = msg(e);
      }
    } finally {
      busyId = null;
    }
  }

  async function onRefreshConflict() {
    conflictMessage = null;
    if (pollController) await refresh(pollController.signal);
  }

  function startRename(w: Workstream) {
    renameId = w.id;
    renameDraft = w.name;
    closeConfirmId = null;
  }

  function cancelRename() {
    renameId = null;
    renameDraft = '';
  }

  async function saveRename() {
    if (!renameId || renameBusy) return;
    const id = renameId;
    renameBusy = true;
    try {
      const base = await apiBase();
      await renameWorkstream(base, id, renameDraft.trim());
      renameId = null;
      if (pollController) await refresh(pollController.signal);
    } catch (e) {
      error = msg(e);
    } finally {
      renameBusy = false;
    }
  }

  async function doClose(id: string) {
    if (closeBusy) return;
    closeBusy = true;
    try {
      const base = await apiBase();
      await closeWorkstream(base, id);
      closeConfirmId = null;
      if (pollController) await refresh(pollController.signal);
    } catch (e) {
      error = msg(e);
    } finally {
      closeBusy = false;
    }
  }
</script>

<div class="wsswitch relative">
  <button
    type="button"
    class="wsswitch-trigger flex items-center gap-1.5 truncate rounded-sm border-1.5 border-trusty-border-strong bg-trusty-card px-2.5 py-1 font-mono text-[11px] uppercase tracking-wide text-trusty-text-secondary disabled:cursor-not-allowed disabled:opacity-50"
    disabled={phase !== 'ready' || !list || list.workstreams.length === 0}
    aria-label="switch workstream"
    aria-expanded={open}
    title={error ?? 'switch workstream'}
    onclick={toggleOpen}
  >
    <span class={`h-1.5 w-1.5 shrink-0 rounded-full ${triggerDotClass()}`}></span>
    <span class="truncate">workstream: {triggerLabel()}</span>
    <span class="text-trusty-text-muted">▾</span>
  </button>

  {#if open}
    <button
      type="button"
      class="fixed inset-0 z-10 cursor-default"
      aria-label="close workstream switcher"
      onclick={closePanel}
    ></button>

    <div
      class="wsswitch-panel absolute left-1/2 top-full z-20 mt-1.5 w-72 -translate-x-1/2 rounded-sm border-1.5 border-trusty-border bg-trusty-raised p-1.5 shadow-lg"
    >
      {#if conflictMessage}
        <div class="mb-1.5 flex items-center justify-between gap-2 rounded-sm bg-status-error/10 px-2 py-1.5 text-[11px] text-status-error">
          <span>{conflictMessage}</span>
          <button
            type="button"
            class="shrink-0 rounded-sm border-1.5 border-status-error px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide"
            onclick={onRefreshConflict}
          >
            refresh
          </button>
        </div>
      {/if}

      {#each list?.workstreams ?? [] as w (w.id)}
        <div class="wsswitch-row flex items-center gap-1 rounded-sm px-1.5 py-1.5 hover:bg-trusty-card">
          {#if renameId === w.id}
            <input
              class="min-w-0 flex-1 rounded-sm border-1.5 border-trusty-primary bg-trusty-card px-1.5 py-0.5 font-mono text-[11px] text-trusty-text"
              bind:value={renameDraft}
              aria-label={`rename ${workstreamLabel(w)}`}
            />
            <button
              type="button"
              class="shrink-0 font-mono text-[10px] uppercase tracking-wide text-trusty-primary"
              disabled={renameBusy}
              onclick={saveRename}
            >
              save
            </button>
            <button
              type="button"
              class="shrink-0 font-mono text-[10px] uppercase tracking-wide text-trusty-text-muted"
              disabled={renameBusy}
              onclick={cancelRename}
            >
              cancel
            </button>
          {:else}
            <button
              type="button"
              class="flex min-w-0 flex-1 items-center gap-1.5 truncate text-left font-mono text-[11px] text-trusty-text disabled:cursor-not-allowed disabled:opacity-60"
              disabled={w.state === 'active' || w.state === 'closed' || busyId === w.id}
              title={w.id}
              onclick={() => onActivateClick(w)}
            >
              <span class={`h-1.5 w-1.5 shrink-0 rounded-full ${workstreamStateDotClass(w.state)}`}></span>
              <span class="truncate">{workstreamLabel(w)}</span>
              {#if w.state === 'active'}
                <span class="shrink-0 text-trusty-primary">active</span>
              {/if}
            </button>

            {#if closeConfirmId === w.id}
              <button
                type="button"
                class="shrink-0 font-mono text-[10px] uppercase tracking-wide text-status-error"
                disabled={closeBusy}
                onclick={() => doClose(w.id)}
              >
                confirm
              </button>
              <button
                type="button"
                class="shrink-0 font-mono text-[10px] uppercase tracking-wide text-trusty-text-muted"
                disabled={closeBusy}
                onclick={() => (closeConfirmId = null)}
              >
                never mind
              </button>
            {:else}
              <button
                type="button"
                class="shrink-0 px-1 font-mono text-[11px] text-trusty-text-muted hover:text-trusty-primary disabled:cursor-not-allowed disabled:opacity-50"
                aria-label={`rename ${workstreamLabel(w)}`}
                onclick={() => startRename(w)}
              >
                ✎
              </button>
              <button
                type="button"
                class="shrink-0 px-1 font-mono text-[11px] text-trusty-text-muted hover:text-status-error disabled:cursor-not-allowed disabled:opacity-50"
                aria-label={`close ${workstreamLabel(w)}`}
                disabled={w.state === 'closed'}
                onclick={() => (closeConfirmId = w.id)}
              >
                ×
              </button>
            {/if}
          {/if}
        </div>
      {:else}
        <p class="px-1.5 py-2 text-[11px] text-trusty-text-muted">no workstreams yet</p>
      {/each}
    </div>
  {/if}
</div>
