<script lang="ts">
  import { onMount } from 'svelte';
  import Sidebar from './components/Sidebar.svelte';
  import ChatView from './components/ChatView.svelte';
  import InputArea from './components/InputArea.svelte';
  import ProjectsView from './components/ProjectsView.svelte';
  import RecapPanel from './components/RecapPanel.svelte';
  import Header from './components/Header.svelte';
  import { invoke, isDesktop, connectEventSource, emitWebEvent, type AppEvent } from './lib/transport';
  import { apiAuthRequired, getCurrentApiToken, setApiToken, addMessage } from './stores/app';
  import { setRecap, type Recap } from './stores/recap';
  // Why: Importing the theme store has the side-effect of running applyTheme()
  // for the persisted theme, ensuring the `dark` class on <html> tracks the
  // user's preference for the lifetime of the app (the inline <head> script
  // sets it pre-mount; this keeps it synchronized as the user toggles).
  import './stores/theme';

  let apiReady = false;
  let apiError = '';
  // Why: Top-level view selector. Chat is the default; Projects shows the
  // /api/projects panel. Kept as a simple tab toggle to avoid pulling in a
  // router for two views. (#341)
  let activeView: 'chat' | 'projects' = 'chat';
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
  let sseStatus: 'idle' | 'connecting' | 'open' | 'reconnecting' = 'idle';

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
   * Test: Run `open-mpm --api --api-token secret`; load the UI; observe the
   * token input form before any chat UI renders.
   */
  async function probeAuthRequired(): Promise<boolean> {
    try {
      const base = (import.meta as ImportMeta & { env: Record<string, string> }).env
        .VITE_OMPM_API ?? '';
      const r = await fetch(`${base}/api/config`);
      if (!r.ok) return false;
      const cfg = (await r.json()) as { auth_required?: boolean };
      return !!cfg.auth_required;
    } catch {
      return false;
    }
  }

  /**
   * Why: The Tauri backend spawns `open-mpm --api --port 8765` as a sidecar
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
   * Why: When a typed server event arrives, route it onto the legacy webBus
   * names so ChatView / TaskHistory / Sidebar pick it up without rewriting
   * each component's listener wiring. Phase B keeps the migration mechanical
   * — Phase C will surface the new event vocabulary directly to components
   * that can render richer state (per-phase timeline, per-agent output).
   * What: Maps a small set of high-signal server events to the existing
   * `task-progress` / `task-complete` / `task-error` web-bus events. Unknown
   * event types are forwarded as `task-progress` with a friendly summary so
   * they show up in the UI rather than being silently dropped.
   * Test: Trigger a workflow run with the API server up; observe Sidebar
   * "Recent tasks" updates without a page refresh and ChatView shows live
   * progress text.
   */
  function bridgeEventToWebBus(ev: AppEvent) {
    switch (ev.type) {
      case 'session_started':
        emitWebEvent('task-progress', {
          task_id: ev.session_id ?? '',
          message: 'Task started…',
        });
        break;
      case 'session_done':
        emitWebEvent('task-complete', {
          id: ev.session_id ?? '',
          status: ev.status ?? 'completed',
          narrative: '',
        });
        break;
      case 'session_cancelled':
        emitWebEvent('task-error', {
          task_id: ev.session_id ?? '',
          error: 'cancelled',
        });
        break;
      case 'pm_thinking':
        emitWebEvent('task-progress', {
          task_id: ev.session_id ?? '',
          message: ev.text ?? '(thinking)',
        });
        break;
      case 'pm_delegating':
        emitWebEvent('task-progress', {
          task_id: ev.session_id ?? '',
          message: `Delegating to ${ev.agent}: ${ev.task_preview ?? ''}`,
        });
        break;
      case 'agent_spawned':
        emitWebEvent('task-progress', {
          task_id: ev.session_id ?? '',
          message: `Agent ${ev.agent} starting…`,
        });
        break;
      case 'agent_message':
        emitWebEvent('task-progress', {
          task_id: ev.session_id ?? '',
          message: `[${ev.agent}] ${ev.text ?? ''}`,
        });
        break;
      case 'agent_done':
        emitWebEvent('task-progress', {
          task_id: ev.session_id ?? '',
          message: `Agent ${ev.agent} done (${ev.status ?? 'ok'})`,
        });
        break;
      case 'agent_failed':
        emitWebEvent('task-error', {
          task_id: ev.session_id ?? '',
          error: `Agent ${ev.agent} failed: ${ev.error ?? ''}`,
        });
        break;
      case 'tool_called':
        emitWebEvent('task-progress', {
          task_id: ev.session_id ?? '',
          message: `Tool ${ev.tool}: ${ev.preview ?? ''}`,
        });
        break;
      case 'phase_started':
        emitWebEvent('task-progress', {
          task_id: ev.session_id ?? '',
          message: `Phase: ${ev.phase}`,
        });
        break;
      case 'phase_done':
        emitWebEvent('task-progress', {
          task_id: ev.session_id ?? '',
          message: `Phase ${ev.phase} ${ev.status ?? 'done'}`,
        });
        break;
      case 'recap_generated': {
        // Why: #371 — surface session recaps in two places: the persistent
        // RecapPanel between chat and input (latest recap per session) and as
        // a banner-style chat message so the recap is preserved in the
        // scrollback. Both consumers read from the recap store; the chat
        // banner is appended via addMessage with role='recap'.
        const sessionId = (ev.session_id ?? '') as string;
        const summary = ((ev as Record<string, unknown>).summary ?? '') as string;
        const rawRows = (ev as Record<string, unknown>).table_rows as unknown;
        const rows: [string, string][] = Array.isArray(rawRows)
          ? (rawRows as unknown[])
              .filter((r): r is [unknown, unknown] => Array.isArray(r) && r.length === 2)
              .map(([s, r]) => [String(s), String(r)])
          : [];
        const recap: Recap = {
          session_id: sessionId,
          summary,
          table_rows: rows,
          received_at: Date.now(),
        };
        setRecap(recap);
        if (sessionId) {
          addMessage(sessionId, {
            id: `recap-${sessionId}-${recap.received_at}`,
            role: 'recap',
            content: summary,
            timestamp: recap.received_at,
            recapRows: rows,
          });
        }
        break;
      }
      case 'ping':
      case 'lag':
        // Diagnostic — no UI action; consumers that care can listen for the
        // typed AppEvent directly via `connectEventSource`.
        break;
      default:
        // Forward unknown events as progress so they're not invisible.
        emitWebEvent('task-progress', {
          task_id: ev.session_id ?? '',
          message: `[${ev.type}]`,
        });
    }
  }

  function startEventStream() {
    if (eventSource || isDesktop()) {
      // Tauri has its own listen() bridge already; web-only path needs SSE.
      return;
    }
    sseStatus = 'connecting';
    eventSource = connectEventSource(
      undefined,
      (ev) => {
        sseStatus = 'open';
        bridgeEventToWebBus(ev);
      },
      () => {
        // The browser will reconnect automatically — surface state for the
        // status pill but don't tear down the EventSource.
        sseStatus = 'reconnecting';
      },
    );
  }

  function stopEventStream() {
    eventSource?.close();
    eventSource = null;
    sseStatus = 'idle';
  }

  // Re-run the start/stop logic whenever apiReady flips so we don't open
  // the stream before the server is up (and don't keep a dead one open if
  // the user logs out / token clears).
  $: if (apiReady) {
    startEventStream();
  } else {
    stopEventStream();
  }

  onMount(() => {
    bootstrap();
    const onUnload = () => stopEventStream();
    window.addEventListener('beforeunload', onUnload);
    return () => {
      window.removeEventListener('beforeunload', onUnload);
      stopEventStream();
    };
  });
</script>

<div class="flex flex-col h-screen w-full relative bg-ompm-light-bg dark:bg-ompm-bg text-ompm-light-text dark:text-ompm-text overflow-hidden">
  <Header />
  <div class="flex flex-1 min-h-0 w-full overflow-hidden">
  {#if $apiAuthRequired && !apiReady}
    <main class="flex flex-1 flex-col items-center justify-center bg-ompm-light-bg dark:bg-ompm-bg px-4">
      <div class="w-full max-w-md rounded-lg border border-ompm-primary/30 bg-ompm-light-surface dark:bg-ompm-surface p-6 shadow-lg">
        <h1 class="mb-2 text-lg font-semibold text-ompm-light-text dark:text-ompm-text">API token required</h1>
        <p class="mb-4 text-sm text-ompm-light-muted dark:text-ompm-text/70">
          The open-mpm API server was started with <code class="font-mono bg-ompm-light-border/50 dark:bg-black/40 rounded px-1 py-0.5 text-xs">--api-token</code>. Paste the token to
          continue. It is saved in this browser only.
        </p>
        <form on:submit|preventDefault={submitToken} class="flex flex-col gap-3">
          <input
            type="password"
            bind:value={tokenInput}
            placeholder="API token"
            autocomplete="off"
            class="rounded-md border border-ompm-light-border dark:border-ompm-primary/30 bg-ompm-light-bg dark:bg-ompm-bg text-ompm-light-text dark:text-ompm-text px-3 py-2 text-sm shadow-sm focus:border-ompm-primary focus:outline-none"
            disabled={probingToken}
          />
          {#if tokenError}
            <p class="text-xs text-red-500 dark:text-red-400">{tokenError}</p>
          {/if}
          <button
            type="submit"
            class="inline-flex items-center justify-center rounded-md bg-ompm-primary px-3 py-2 text-sm font-medium text-white shadow-sm hover:bg-ompm-primary/80 disabled:cursor-not-allowed disabled:bg-ompm-light-surface dark:disabled:bg-ompm-surface disabled:text-ompm-light-muted dark:disabled:text-ompm-text/40"
            disabled={probingToken || !tokenInput.trim()}
          >
            {probingToken ? 'Verifying…' : 'Continue'}
          </button>
        </form>
      </div>
    </main>
  {:else if apiError}
    <!-- Full-screen error state: visible regardless of theme, never dark-on-dark -->
    <main class="flex flex-1 flex-col items-center justify-center bg-ompm-light-bg dark:bg-ompm-bg px-4">
      <div class="w-full max-w-md rounded-lg border border-red-500/40 bg-ompm-light-surface dark:bg-ompm-surface p-6 shadow-lg">
        <h1 class="mb-2 text-lg font-semibold text-red-500 dark:text-red-400">API server error</h1>
        <p class="mb-4 text-sm text-ompm-light-text/80 dark:text-ompm-text/80 leading-relaxed break-words">{apiError}</p>
        <p class="text-xs text-ompm-light-muted dark:text-ompm-text/50">
          Make sure <code class="font-mono bg-ompm-light-border/50 dark:bg-black/40 rounded px-1 py-0.5">open-mpm --api</code> is
          running, then reload the page.
        </p>
        <button
          type="button"
          class="mt-4 inline-flex items-center justify-center rounded-md bg-ompm-primary px-3 py-2 text-sm font-medium text-white shadow-sm hover:bg-ompm-primary/80"
          on:click={() => window.location.reload()}
        >
          Reload
        </button>
      </div>
    </main>
  {:else if !apiReady}
    <!-- Full-screen loading state: spinning indicator with status text, never blank -->
    <main class="flex flex-1 flex-col items-center justify-center bg-ompm-light-bg dark:bg-ompm-bg px-4">
      <div class="flex flex-col items-center gap-4 text-ompm-light-text dark:text-ompm-text">
        <svg class="h-8 w-8 animate-spin text-ompm-primary" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24">
          <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
          <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"></path>
        </svg>
        <p class="text-sm font-medium text-ompm-light-text dark:text-ompm-text">Connecting to API server…</p>
        <p class="text-xs text-ompm-light-muted dark:text-ompm-text/50">open-mpm --api on port {desktop ? 8765 : 7654}</p>
      </div>
    </main>
  {:else}
    <Sidebar {apiReady} {apiError} />
    <main class="flex flex-1 flex-col bg-ompm-light-bg dark:bg-ompm-bg">
      <nav class="flex items-center gap-1 border-b border-ompm-light-border dark:border-ompm-border px-4 py-1">
        <button
          type="button"
          class="rounded-md px-3 py-1 text-xs font-medium transition-colors {activeView === 'chat'
            ? 'bg-ompm-primary/20 text-ompm-primary'
            : 'text-ompm-light-muted dark:text-ompm-text/60 hover:bg-ompm-primary/10'}"
          on:click={() => (activeView = 'chat')}
        >
          Chat
        </button>
        <button
          type="button"
          class="rounded-md px-3 py-1 text-xs font-medium transition-colors {activeView === 'projects'
            ? 'bg-ompm-primary/20 text-ompm-primary'
            : 'text-ompm-light-muted dark:text-ompm-text/60 hover:bg-ompm-primary/10'}"
          on:click={() => (activeView = 'projects')}
        >
          Projects
        </button>
      </nav>
      {#if activeView === 'chat'}
        <ChatView />
        <RecapPanel />
        <InputArea />
      {:else}
        <ProjectsView on:navigate={(e) => (activeView = e.detail.view)} />
      {/if}
    </main>
  {/if}
  </div>
  <span
    class="absolute top-14 right-3 z-30 text-[10px] px-2 py-0.5 rounded-full {desktop
      ? 'bg-ompm-primary/20 text-ompm-primary'
      : 'bg-ompm-amber/20 text-ompm-amber'}"
    title={desktop ? 'Running inside Tauri (IPC)' : 'Running in browser (HTTP /api)'}
  >
    {desktop ? '⊞ Desktop' : '⟳ Web'}
  </span>
</div>
