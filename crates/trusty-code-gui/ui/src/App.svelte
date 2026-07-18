<script lang="ts">
  // Why: Minimal scaffold shell (issue #2983, DOC-39) — proves the desktop
  // app connects to the tcode daemon and gives the next UI slice a layout to
  // extend. Phase 1 adds the status bar (DOC-39 §6.2), the 8b session
  // monitor card (DOC-39 §4.6, refs #2983), the 10d Search tab (DOC-39
  // §4.7, refs #3072), and `CreateSessionForm` — the 7a folder picker +
  // task-input flow (DOC-39 §4.2.1, §6.2 item 6) — closing the gap that
  // made the GUI observe-and-cancel only: there was previously no way to
  // start a session from the desktop shell at all. `body`'s prior
  // `ActivityPlaceholder` explicitly said its purpose was "session list /
  // activity view... coming once GET /sessions lands" — that has now
  // landed and `SessionMonitor` covers "what's happening in the active
  // session" more concretely than the placeholder's static text did, so it
  // is REPLACED here rather than shown alongside it (a still-unbuilt
  // session-LIST/picker view, distinct from monitoring the one active
  // session, is a separate Phase 2+ surface per DOC-39 §6.3, not something
  // this placeholder was actually standing in for once GET /sessions is
  // live).
  // What: Renders a header, a `.body` region (the daemon HealthPanel smoke
  // connection, the create-session form, the session monitor card — new
  // session before monitor since it's the entry action and the monitor
  // reflects the result — then the search audit tab), and `StatusBar` — the
  // readiness+budget chrome — as a DIRECT SIBLING of `.body`, never nested
  // inside it. DOC-39 §8.1 / AC-18.1 calls this out explicitly: nesting
  // `.statusbar` inside `.body` has regressed the wireframe twice ("This bit
  // us twice… assert it in a test") because it steals the body's row width.
  // `App.test.ts` asserts the DOM invariant this markup encodes.
  // Test: Launch under Tauri or `pnpm dev` in a browser — both show the same
  // connected/disconnected state (DOC-39 §2.1 web/Tauri parity) plus the
  // status bar's readiness indicator, the create-session form, the session
  // monitor card, and the search tab. `App.test.ts::statusbar-is-sibling-of-body`
  // pins the structural invariant; `App.test.ts` also pins each card's
  // inside-`.body` placement (create-session form, session monitor, search
  // tab).
  import CreateSessionForm from './components/CreateSessionForm.svelte';
  import HealthPanel from './components/HealthPanel.svelte';
  import SearchTab from './components/SearchTab.svelte';
  import SessionMonitor from './components/SessionMonitor.svelte';
  import StatusBar from './components/StatusBar.svelte';
</script>

<main class="app flex min-h-screen flex-col bg-trusty-surface text-trusty-text">
  <header class="p-6 pb-0">
    <h1 class="text-lg font-semibold">trusty-code</h1>
    <p class="text-xs text-trusty-text/60">Desktop shell — thin client over the tcode daemon</p>
  </header>

  <div class="body flex flex-1 flex-col gap-4 p-6">
    <HealthPanel />
    <CreateSessionForm />
    <SessionMonitor />
    <SearchTab />
  </div>

  <StatusBar />
</main>
