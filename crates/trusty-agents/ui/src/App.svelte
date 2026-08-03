<script lang="ts">
  import { onMount } from 'svelte';
  import { get } from 'svelte/store';
  import Sidebar from './components/Sidebar.svelte';
  import Header from './components/Header.svelte';
  // #3894: the Chat view's whole composition (chat column + recap rail +
  // Slack mirror + the agent-configuration takeover that covers them) lives
  // in `ChatPane` — see that component's doc comment for why the takeover
  // has to be a sibling of the group rather than nested in the chat column.
  import ChatPane from './components/ChatPane.svelte';
  // #4404: the landing view — a card per assistant instance + Concierge +
  // create. Selection writes `activeAgentId`, which persists itself (#4281).
  import AssistantPicker from './components/AssistantPicker.svelte';
  import EventsView from './components/EventsView.svelte';
  // #4098 (COST-09): the Costs tab — spend by agent/model/day from
  // `GET /api/costs`. Owns its own fetch; see the component's doc comment.
  import CostsView from './components/CostsView.svelte';
  // #3819: `ProjectsView`/`PersonalityPanel` are no longer routed — Bob's
  // nav reshape drops the Projects/Personality top tabs entirely. Left
  // unimported/unrouted rather than deleted pending a decision on their
  // dropped capability (project-registration-by-path + session attach/
  // resume/pause/kill; the "+ New agent from overlay" flow) — see issue
  // #3819 and the PR body for what's lost vs. relocated (Personality's
  // "edit this agent's prose" half moves to `AgentConfigPanel`, reachable
  // via `ChatHeader`'s gear icon).
  // #3752: live Slack conversation mirror — the SSE bridge feeds these two
  // event kinds into the store the SlackMirror pane renders.
  import { pushSlackEvent } from './lib/slack-mirror';
  // #3759: the SSE→web-bus routing table (Events-tab log, workflow store,
  // Slack mirror and recap fan-out included) now lives in `lib/eventBridge.ts`
  // so it is unit-testable; it used to be an inline `bridgeEventToWebBus`
  // here, with no automated coverage at all.
  import { bridgeEventToWebBus } from './lib/eventBridge';
  import { invoke, isDesktop, connectEventSource, listenEvent, type AppEvent } from './lib/transport';
  import { shouldRefetchCatalogs } from './lib/catalogRefetch';
  import {
    apiAuthRequired,
    getCurrentApiToken,
    setApiToken,
    fetchAgentCatalog,
    fetchModelCatalog,
    refreshOverlayAgents,
  } from './stores/app';
  import { requestExitConfigPane } from './stores/configPane';
  // Why (#3217): parallel structured-data sink — see stores/workflow.ts doc
  // comment for the full rationale. Additive to the flattened webBus path
  // in `lib/eventBridge.ts`, never a replacement.
  import { applyTaskResult, resetWorkflow, workflowState } from './stores/workflow';
  // Why: Importing the theme store has the side-effect of running applyTheme()
  // for the persisted theme, ensuring the `dark` class on <html> tracks the
  // user's preference for the lifetime of the app (the inline <head> script
  // sets it pre-mount; this keeps it synchronized as the user toggles).
  import './stores/theme';

  let apiReady = false;
  let apiError = '';
  // Why (#3819): Chat/Projects/Personality collapses to Chat/Events per
  // Bob's nav reshape. The unsaved-Personality-buffer guard this used to
  // gate is gone with the Personality tab itself — `AgentConfigPanel`
  // (reached via `ChatHeader`'s gear icon) saves each section independently
  // on its own explicit Save button, so there is no cross-tab discard risk
  // to guard against here anymore.
  // #4098: 'costs' added for the Costs tab (COST-09).
  // #4404: 'assistants' added AND made the DEFAULT — the app opens on "who am
  // I working with" rather than dropping the user into a conversation whose
  // participant they never chose. Picking a card selects the assistant and
  // moves to Chat; the tab stays available so the picker is re-reachable
  // without a relaunch. (`ProjectsView` deliberately gains no entry: #3819
  // dropped it from the nav and epic #4355 rebuilds that surface.)
  type View = 'assistants' | 'chat' | 'events' | 'costs';
  let activeView: View = 'assistants';

  function switchView(view: View) {
    // #3894: leaving Chat abandons any open configuration takeover, so the
    // top-level tabs always mean what they say — coming back to Chat shows
    // chat, never a config surface the user left behind minutes ago.
    // PR #3895 code-critic HIGH-1: this is also an EXIT path — leaving Chat
    // unmounts the panel — so it goes through the same guard. When the panel
    // holds unsaved edits the request raises its confirm and returns false;
    // we stay on Chat rather than tearing the edit out from under it.
    if (view !== 'chat' && !requestExitConfigPane()) return;
    activeView = view;
  }
  let tokenInput = '';
  let tokenError = '';
  let probingToken = false;

  // Why: A single, app-lifetime EventSource pulls real-time PM/agent/workflow
  // telemetry from `/api/events` (#192 Phase B). We translate the server's
  // typed events into the same `task-progress` / `task-complete` /
  // `task-error` web-bus names that ChatView and TaskHistory already listen
  // on, so existing components light up without changes. Stored at module
  // scope so it survives Svelte's hot-reload re-renders during dev.
  // What: Opens on first apiReady=true, stays open until the page is closed.
  // The EventSource API auto-reconnects on transport failure with browser-
  // native exponential backoff so we don't have to.
  let eventSource: EventSource | null = null;

  // Why: Make the active transport visible at a glance so contributors and
  // testers can tell whether they're in the Tauri desktop shell (full IPC)
  // or a plain browser tab hitting `--api` over HTTP. Saves debugging
  // confusion when events behave differently between modes.
  // What: A small pill in the top-right of the app showing "Desktop" or
  // "Web". Computed once at module load — the runtime cannot switch.
  // Test: `pnpm tauri dev` shows the indigo Desktop pill; `pnpm dev` in a
  // browser shows the amber Web pill.
  const desktop = isDesktop();

  /**
   * Why: When the server is launched with `--api-token`, every request without
   * a Bearer token returns 401, but the UI used to show no feedback — users
   * just saw silent failures. Probing `/api/config` (which is unauthenticated)
   * lets us detect the requirement up-front and prompt for a token.
   * What: Calls `GET /api/config` and returns whether auth is required.
   * Returns false on network errors so we don't block a working server.
   * Test: Run `tagent --api --api-token secret`; load the UI; observe the
   * token input form before any chat UI renders.
   */
  async function probeAuthRequired(): Promise<boolean> {
    try {
      const base = (import.meta as ImportMeta & { env: Record<string, string> }).env
        .VITE_TAGENT_API ?? '';
      const r = await fetch(`${base}/api/config`);
      if (!r.ok) return false;
      const cfg = (await r.json()) as { auth_required?: boolean };
      return !!cfg.auth_required;
    } catch {
      return false;
    }
  }

  /**
   * Why: The Tauri backend spawns `tagent --api --port 8765` as a sidecar
   * when the window opens so the REST server is already listening by the
   * time the user sends their first message. We call it once on mount; if
   * we're running under plain Vite, the command falls back to a no-op and
   * we assume the user has started the server manually.
   * What: Kicks off `ensure_api_server(8765)` and flips `apiReady` when the
   * health check succeeds. After health is healthy, probes `/api/config` to
   * detect whether the server requires an API token; if so and no token is
   * set, holds `apiReady=false` until the user supplies one.
   * Test: Start the Tauri app, observe that within ~2s the sidebar header
   * stops showing "Starting…" and switches to "API ready". With
   * `--api-token`, observe a token input form appears instead of the chat.
   */
  async function bootstrap() {
    try {
      await invoke('ensure_api_server', { port: 8765 });
    } catch (e) {
      apiError = `ensure_api_server failed: ${e}`;
    }
    // Probe health up to 40 attempts (20s). Re-invoke ensure_api_server on
    // every even attempt so a dead sidecar gets respawned (the Rust handler
    // now clears the dead-child slot before respawning).
    for (let i = 0; i < 40; i++) {
      try {
        const ok = await invoke<boolean>('check_health');
        if (ok) {
          // Server is up — now check whether it requires a token.
          const authRequired = await probeAuthRequired();
          if (authRequired && !getCurrentApiToken()) {
            apiAuthRequired.set(true);
            return; // Wait for user to submit a token via the form.
          }
          apiAuthRequired.set(authRequired);
          apiReady = true;
          return;
        }
      } catch {
        // Keep trying — server may still be binding its socket.
      }
      await new Promise((r) => setTimeout(r, 500));
      // Periodically re-invoke ensure_api_server in case the sidecar died and
      // needs to be respawned. The Rust handler is idempotent: it skips
      // respawn when the child is still alive.
      if (i % 4 === 3) {
        try {
          await invoke('ensure_api_server', { port: 8765 });
        } catch {
          // Ignore; health loop will surface the failure.
        }
      }
    }
    if (!apiError) apiError = 'API server did not become healthy within 20s';
  }

  /**
   * Why: When auth is required and the user submits a token, we need to
   * verify it works (the user could have pasted a typo) before letting them
   * into the chat. We do this by re-probing `/api/config` with the token
   * applied — but `/api/config` is unauthenticated, so we instead hit
   * `/api/tasks` which IS protected; a 200 confirms the token is valid.
   * What: Persists the token, calls `list_tasks`, sets `apiReady=true` on
   * success or surfaces an error message on 401.
   * Test: Enter a wrong token, expect "Invalid token" message; enter the
   * correct token, expect the chat UI to render.
   */
  async function submitToken() {
    const t = tokenInput.trim();
    if (!t) {
      tokenError = 'Token is required';
      return;
    }
    probingToken = true;
    tokenError = '';
    setApiToken(t);
    try {
      await invoke('list_tasks');
      apiReady = true;
    } catch (e) {
      tokenError = `Invalid token: ${e}`;
      setApiToken(''); // clear the bad token so the form stays usable
    } finally {
      probingToken = false;
    }
  }

  /**
   * Why (#3257 code-critic HIGH): `workflowState.resetWorkflow()` previously
   * only fired on the SSE `session_started` event — but Tauri desktop mode
   * never opens the SSE stream (`startEventStream()` below no-ops when
   * `isDesktop()`; "Tauri has its own listen() bridge already"), so
   * `handleWorkflowEvent` never runs there. Without this, a new desktop task
   * would keep showing the PREVIOUS task's phase checklist / agents / files
   * until the new task's own `task-complete` finally overwrote them —
   * actively misleading mid-run. `task-progress` is emitted in BOTH modes
   * (browser fallback's own polling loop and the Tauri `send_message`
   * command) and its very first firing for a task always carries that
   * task's id, so it's the earliest per-task signal available without
   * touching `InputArea.svelte` or `src-tauri/**`.
   * What: Resets + adopts a new task id the moment it's first seen. No-op
   * when the id matches what's already tracked (the common case — most
   * `task-progress` ticks repeat the same id) so this never clobbers
   * in-flight browser/SSE phase data for the task already running; in
   * browser mode `session_started` still performs its own reset as before
   * (this only changes behavior for desktop, where it was previously a
   * no-op because nothing ever called `resetWorkflow()`).
   * Test: Manual — start a desktop task, let it reach IMPLEMENT, submit a
   * second task before the first's `task-complete` arrives, confirm the
   * phase card clears instead of showing the first task's stale phases.
   */
  function maybeAdoptNewTask(taskId: string | null | undefined) {
    if (!taskId) return;
    const current = get(workflowState);
    if (current.taskId !== taskId) {
      resetWorkflow();
      workflowState.update((s) => ({ ...s, taskId, status: 'running' }));
    }
  }

  function startEventStream() {
    if (eventSource || isDesktop()) {
      // Tauri has its own listen() bridge already; web-only path needs SSE.
      return;
    }
    eventSource = connectEventSource(
      undefined,
      (ev) => {
        bridgeEventToWebBus(ev);
      },
      () => {
        // The browser will reconnect automatically — nothing to tear down.
      },
    );
  }

  function stopEventStream() {
    eventSource?.close();
    eventSource = null;
  }

  // Re-run the start/stop logic whenever apiReady flips so we don't open
  // the stream before the server is up (and don't keep a dead one open if
  // the user logs out / token clears).
  $: if (apiReady) {
    startEventStream();
  } else {
    stopEventStream();
  }

  // Agent-picker cold-start race (owner report 2026-07-23):
  // AgentSwitcher/ModelSwitcher fetch their catalogs in their own `onMount`,
  // but `<Header>` — and therefore both pickers — renders unconditionally,
  // before `apiReady`. On a cold start the sidecar isn't listening yet, so
  // that first fetch fails, the catalog stores stay empty, and the pickers
  // show only their built-in default ("Assistant" / "Default") for the whole
  // session with no retry — Izzie/CTO Bot never become selectable. Re-driving
  // the catalog loads the moment the API becomes healthy backfills the
  // already-mounted pickers via their reactive stores. `apiReady` is set true
  // exactly once per app lifetime and never reset, so this fires once; the
  // edge-detector (`shouldRefetchCatalogs`) keeps it from re-running on any
  // unrelated reactive re-evaluation, and would correctly re-fire if a future
  // change ever reset `apiReady` false→true on reconnect.
  let prevApiReady = false;
  function refetchPickerCatalogsOnReady(ready: boolean) {
    if (shouldRefetchCatalogs(prevApiReady, ready)) {
      fetchAgentCatalog().catch((e) =>
        console.error('[App] fetchAgentCatalog failed:', e),
      );
      fetchModelCatalog().catch((e) =>
        console.error('[App] fetchModelCatalog failed:', e),
      );
      refreshOverlayAgents();
    }
    prevApiReady = ready;
  }
  $: refetchPickerCatalogsOnReady(apiReady);

  onMount(() => {
    bootstrap();
    const onUnload = () => stopEventStream();
    window.addEventListener('beforeunload', onUnload);
    // #3217: `task-complete`'s payload is the full `PmResponse` in Tauri
    // desktop mode (phases_completed/files_modified/metadata included) and
    // a narrower {id,status,narrative} shape from the browser fallback;
    // `applyTaskResult` merges whichever fields are present. This is the
    // only source of per-phase elapsed/cost/note and the files/tokens
    // sections in either transport mode.
    let unlistenWorkflowComplete: (() => void) | null = null;
    listenEvent('task-complete', applyTaskResult).then((fn) => {
      unlistenWorkflowComplete = fn;
    });
    // #3257 code-critic HIGH: the earliest per-task signal available in
    // both transport modes — see `maybeAdoptNewTask` doc comment above.
    let unlistenWorkflowProgress: (() => void) | null = null;
    listenEvent<{ task_id?: string }>('task-progress', (p) => {
      maybeAdoptNewTask(p?.task_id);
    }).then((fn) => {
      unlistenWorkflowProgress = fn;
    });
    // #3752: desktop parity for the Slack mirror. In browser mode
    // `bridgeEventToWebBus` calls `pushSlackEvent` directly off the SSE stream;
    // the Tauri shell skips that browser bridge (`startEventStream` early-
    // returns on `isDesktop()`), so its Rust `sse_bridge` re-emits the two
    // Slack kinds as a `slack-event` Tauri event that we fold in here. This
    // listener is a harmless no-op in browser mode (nothing emits `slack-event`
    // on the web bus), so exactly one push happens per event in each transport.
    let unlistenSlack: (() => void) | null = null;
    listenEvent<AppEvent>('slack-event', (ev) => {
      pushSlackEvent(ev);
    }).then((fn) => {
      unlistenSlack = fn;
    });
    return () => {
      window.removeEventListener('beforeunload', onUnload);
      unlistenWorkflowComplete?.();
      unlistenWorkflowProgress?.();
      unlistenSlack?.();
      stopEventStream();
    };
  });
</script>

<div class="flex flex-col h-screen w-full relative bg-foundry-light-bg dark:bg-foundry-bg text-foundry-light-text dark:text-foundry-text overflow-hidden">
  <Header {activeView} {apiReady} on:switch-view={(e) => switchView(e.detail.view)} />
  <div class="flex flex-1 min-h-0 w-full overflow-hidden">
  {#if $apiAuthRequired && !apiReady}
    <main class="flex flex-1 flex-col items-center justify-center bg-foundry-light-bg dark:bg-foundry-bg px-4">
      <div class="w-full max-w-md rounded-lg border border-foundry-light-primary/30 dark:border-foundry-primary/30 bg-foundry-light-surface dark:bg-foundry-surface p-6 shadow-lg">
        <h1 class="mb-2 text-lg font-semibold text-foundry-light-text dark:text-foundry-text">API token required</h1>
        <p class="mb-4 text-sm text-foundry-light-muted dark:text-foundry-text/70">
          The tagent API server was started with <code class="font-mono bg-foundry-light-border/50 dark:bg-black/40 rounded px-1 py-0.5 text-xs">--api-token</code>. Paste the token to
          continue. It is saved in this browser only.
        </p>
        <form on:submit|preventDefault={submitToken} class="flex flex-col gap-3">
          <input
            type="password"
            bind:value={tokenInput}
            placeholder="API token"
            autocomplete="off"
            class="rounded-md border border-foundry-light-border dark:border-foundry-primary/30 bg-foundry-light-bg dark:bg-foundry-bg text-foundry-light-text dark:text-foundry-text px-3 py-2 text-sm shadow-sm focus:border-foundry-light-primary dark:focus:border-foundry-primary focus:outline-none"
            disabled={probingToken}
          />
          {#if tokenError}
            <p class="text-xs text-red-500 dark:text-red-400">{tokenError}</p>
          {/if}
          <button
            type="submit"
            class="inline-flex items-center justify-center rounded-md bg-foundry-light-primary dark:bg-foundry-primary px-3 py-2 text-sm font-medium text-white shadow-sm hover:bg-foundry-light-primary/80 dark:hover:bg-foundry-primary/80 disabled:cursor-not-allowed disabled:bg-foundry-light-surface dark:disabled:bg-foundry-surface disabled:text-foundry-light-muted dark:disabled:text-foundry-text/40"
            disabled={probingToken || !tokenInput.trim()}
          >
            {probingToken ? 'Verifying…' : 'Continue'}
          </button>
        </form>
      </div>
    </main>
  {:else if apiError}
    <!-- Full-screen error state: visible regardless of theme, never dark-on-dark -->
    <main class="flex flex-1 flex-col items-center justify-center bg-foundry-light-bg dark:bg-foundry-bg px-4">
      <div class="w-full max-w-md rounded-lg border border-red-500/40 bg-foundry-light-surface dark:bg-foundry-surface p-6 shadow-lg">
        <h1 class="mb-2 text-lg font-semibold text-red-500 dark:text-red-400">API server error</h1>
        <p class="mb-4 text-sm text-foundry-light-text/80 dark:text-foundry-text/80 leading-relaxed break-words">{apiError}</p>
        <p class="text-xs text-foundry-light-muted dark:text-foundry-text/50">
          Make sure <code class="font-mono bg-foundry-light-border/50 dark:bg-black/40 rounded px-1 py-0.5">tagent --api</code> is
          running, then reload the page.
        </p>
        <button
          type="button"
          class="mt-4 inline-flex items-center justify-center rounded-md bg-foundry-light-primary dark:bg-foundry-primary px-3 py-2 text-sm font-medium text-white shadow-sm hover:bg-foundry-light-primary/80 dark:hover:bg-foundry-primary/80"
          on:click={() => window.location.reload()}
        >
          Reload
        </button>
      </div>
    </main>
  {:else if !apiReady}
    <!-- Full-screen loading state: spinning indicator with status text, never blank -->
    <main class="flex flex-1 flex-col items-center justify-center bg-foundry-light-bg dark:bg-foundry-bg px-4">
      <div class="flex flex-col items-center gap-4 text-foundry-light-text dark:text-foundry-text">
        <svg class="h-8 w-8 animate-spin text-foundry-light-primary dark:text-foundry-primary" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24">
          <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
          <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"></path>
        </svg>
        <p class="text-sm font-medium text-foundry-light-text dark:text-foundry-text">Connecting to API server…</p>
        <p class="text-xs text-foundry-light-muted dark:text-foundry-text/50">tagent --api on port {desktop ? 8765 : 7654}</p>
      </div>
    </main>
  {:else}
    <Sidebar {apiReady} {apiError} />
    <main class="flex flex-1 flex-col bg-foundry-light-bg dark:bg-foundry-bg">
      <!-- #3220: the top-level Chat/Events tab nav lives in <Header/>, which
           dispatches `switch-view` back up to `switchView()` above. #3819:
           the chat pane additionally gets its OWN header (`ChatHeader`) —
           active-agent title + inline selector + gear config button — since
           that state (which agent) is scoped to the chat view, not the
           whole app. -->
      {#if activeView === 'assistants'}
        <!-- #4404: the landing picker. `select` carries the decoded dispatch
             id (null = Concierge) purely for symmetry — the component has
             already written it to `activeAgentId`, so App only has to move on
             to the conversation. -->
        <AssistantPicker on:select={() => switchView('chat')} />
      {:else if activeView === 'chat'}
        <ChatPane />
      {:else if activeView === 'costs'}
        <!-- #4098 (COST-09): Costs tab. -->
        <CostsView />
      {:else}
        <EventsView />
      {/if}
    </main>
  {/if}
  </div>
</div>
