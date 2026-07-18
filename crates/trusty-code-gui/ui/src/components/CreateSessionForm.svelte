<script lang="ts">
  // Why: DOC-39's biggest gap called out for this slice — the GUI could only
  // observe and cancel a session, never create one. This is the create-
  // session flow: the 7a folder picker (§4.2.1) plus the minimal task-input
  // form needed to call `POST /sessions` (`session.create`, REST Slice 3,
  // `crate::serve::rest::sessions_write`). Two daemon routes cover the whole
  // slice — no Tauri command, no native dialog (barred by §2.1 C-4, see
  // `lib/create-session.ts`'s module doc), identical behavior in web and
  // Tauri per C-3.
  //
  // **Picker interaction model.** Mirrors 7a's own shape: browsing a
  // directory lists its CHILDREN as the selectable candidates (each row a
  // future project root), not the directory itself — the breadcrumb is where
  // you are, the rows are what you can bind. A row's directory arrow
  // (`▸ open`) descends into it to browse further; its `use` button binds it
  // as `selectedProject` without navigating. Only entries `GET /fs` already
  // returned are selectable, so by construction every `selectedProject` has
  // already passed the exists+is-dir check this task's brief asks for — no
  // second validation round-trip is needed at submit time (`is_git_repo`
  // comes along for free, feeding DOC-39 §4.2's binding-state label).
  // Leaving the selection cleared (or clicking `clear`) submits projectless
  // (`project` omitted from the body) — AC-2.1 mandates this MUST be
  // supported, not treated as an error state.
  //
  // What: Two independent `$effect`s, same `AbortController`-per-lifetime
  // shape `SessionMonitor.svelte`/`StatusBar.svelte` already establish: one
  // re-fetches `GET /fs?path=..` whenever `browsePath` changes (aborting the
  // previous in-flight listing fetch — a fast double-click through several
  // directories must not let a stale response win the race), the other only
  // aborts `submitController` on unmount (`POST /sessions` is user-triggered,
  // not polled, so it needs no interval). The prior listing stays on screen
  // while a new one loads (`listingPhase === 'loading'` renders a small
  // inline cue rather than blanking the rows) to avoid picker flicker while
  // navigating. Submit is gated by `canSubmitCreate` (non-empty task, not
  // already in flight — `lib/create-session.ts`), and the button's `disabled`
  // binding is the only double-submit guard needed since there is no
  // separate confirm step for creation (unlike `SessionMonitor`'s cancel).
  // A successful `201` clears `task`/`selectedProject` and shows the new
  // session's id when the body carries one (a generic "session created"
  // otherwise — the 201 status, not the body, is authoritative). Both
  // response bodies pass runtime shape guards (`isDirListing` /
  // `extractSessionId`) before touching reactive state, so a shape-invalid
  // 200/201 degrades to an inline message instead of throwing out of this
  // unconditionally-mounted component and crashing the shell.
  // The session itself becomes visible through the existing
  // `GET /sessions` pollers (`StatusBar`/`SessionMonitor`'s `pickActiveSession`)
  // on their next tick — this component does not need its own poll.
  // Test: `create-session.test.ts` covers the pure gating/body-construction
  // logic; `CreateSessionForm.test.ts` covers the form's disabled/enabled
  // submit states, the no-double-submit guard, and picker navigation.
  import { apiBase } from '../lib/api-config';
  import {
    bindingLabel,
    buildCreateBody,
    canSubmitCreate,
    describeFsError,
    extractSessionId,
    isDirListing,
    type DirEntryInfo,
    type DirListing,
    type ProjectSelection,
    type SubmitPhase,
  } from '../lib/create-session';

  type ListingPhase = 'loading' | 'error' | 'ready';

  let browsePath = $state<string | null>(null);
  let listing = $state<DirListing | null>(null);
  let listingPhase = $state<ListingPhase>('loading');
  let listingError = $state<string | null>(null);

  let selectedProject = $state<ProjectSelection | null>(null);
  let task = $state('');

  let submitPhase = $state<SubmitPhase>('idle');
  let submitError = $state<string | null>(null);
  let successMessage = $state<string | null>(null);

  let submitController: AbortController | null = null;

  async function loadListing(path: string | null, signal: AbortSignal) {
    listingPhase = 'loading';
    let base: string;
    try {
      base = await apiBase();
    } catch (e) {
      if (!signal.aborted) {
        listingPhase = 'error';
        listingError = e instanceof Error ? e.message : String(e);
      }
      return;
    }
    if (signal.aborted) return;

    const qs = path ? `?path=${encodeURIComponent(path)}` : '';
    try {
      const res = await fetch(`${base}/fs${qs}`, { signal });
      if (!res.ok) {
        if (!signal.aborted) {
          listingPhase = 'error';
          listingError = describeFsError(res.status);
        }
        return;
      }
      const body: unknown = await res.json();
      if (signal.aborted) return;
      if (!isDirListing(body)) {
        // A 200 with a shape-invalid body (schema drift, proxy, future
        // daemon) must degrade to the error line, not reach the
        // `listing.entries` $derived and throw — this component is mounted
        // unconditionally, so an uncaught throw here takes down the shell.
        listingPhase = 'error';
        listingError = 'malformed response from daemon';
        return;
      }
      listing = body;
      listingPhase = 'ready';
      listingError = null;
    } catch (e) {
      if (!signal.aborted) {
        listingPhase = 'error';
        listingError = e instanceof Error ? e.message : String(e);
      }
    }
  }

  function openEntry(entry: DirEntryInfo) {
    if (!entry.is_dir) return;
    browsePath = entry.path;
  }

  function goUp() {
    if (listing?.parent) browsePath = listing.parent;
  }

  function selectEntry(entry: DirEntryInfo) {
    selectedProject = { path: entry.path, displayPath: entry.name, isGitRepo: entry.is_git_repo };
  }

  function clearSelection() {
    selectedProject = null;
  }

  async function submit() {
    if (!canSubmitCreate(task, submitPhase)) return;
    submitPhase = 'submitting';
    submitError = null;
    successMessage = null;

    const controller = new AbortController();
    submitController = controller;
    try {
      const base = await apiBase();
      if (controller.signal.aborted) return;

      const body = buildCreateBody(task, selectedProject);
      const res = await fetch(`${base}/sessions`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
        signal: controller.signal,
      });
      if (controller.signal.aborted) return;

      if (res.status !== 201) {
        const errBody = (await res.json().catch(() => null)) as {
          error?: { message?: string };
        } | null;
        submitError = errBody?.error?.message ?? `HTTP ${res.status}`;
        return;
      }

      // The 201 status is authoritative — the session exists even if the
      // body is missing/malformed (same guard rationale as `isDirListing`),
      // so a bad body degrades the message to a generic one, never an error.
      const created: unknown = await res.json().catch(() => null);
      if (controller.signal.aborted) return;
      const id = extractSessionId(created);
      successMessage = id ? `session created — ${id.slice(0, 8)}` : 'session created';
      task = '';
      selectedProject = null;
    } catch (e) {
      if (!controller.signal.aborted) {
        submitError = e instanceof Error ? e.message : String(e);
      }
    } finally {
      if (!controller.signal.aborted) submitPhase = 'idle';
      if (submitController === controller) submitController = null;
    }
  }

  $effect(() => {
    const path = browsePath;
    const controller = new AbortController();
    void loadListing(path, controller.signal);
    return () => controller.abort();
  });

  $effect(() => {
    // Unmount-only teardown for the submit request — creation is
    // user-triggered, not polled, so this effect has no reactive deps.
    return () => {
      submitController?.abort();
    };
  });

  let dirEntries = $derived(listing?.entries.filter((e) => e.is_dir) ?? []);
</script>

<section class="mt-4 rounded-lg border border-trusty-border bg-trusty-surface/60 p-4">
  <h2 class="text-sm font-semibold text-trusty-text">new session</h2>

  <div class="mt-3">
    <p class="text-xs text-trusty-text/60">project folder</p>

    {#if listingPhase === 'error' && !listing}
      <p class="mt-1 text-xs text-status-error">
        daemon unreachable{listingError ? ` — ${listingError}` : ''}
      </p>
    {:else if listing}
      <div class="mt-1 flex items-center gap-2 text-xs text-trusty-text/70">
        <button
          type="button"
          disabled={!listing.parent}
          onclick={goUp}
          class="rounded border border-trusty-border px-1.5 py-0.5 disabled:cursor-not-allowed disabled:opacity-30"
        >
          ↑ up
        </button>
        <span class="truncate font-mono" title={listing.path}>{listing.display_path}</span>
        {#if listingPhase === 'loading'}
          <span class="text-trusty-text/30">loading…</span>
        {/if}
      </div>

      <ul class="mt-2 max-h-40 space-y-1 overflow-y-auto text-xs">
        {#each dirEntries as entry (entry.path)}
          <li class="flex items-center justify-between gap-2 rounded px-1.5 py-1 hover:bg-trusty-border/10">
            <button
              type="button"
              class="flex flex-1 items-center gap-1.5 truncate text-left"
              onclick={() => openEntry(entry)}
            >
              <span class="truncate">{entry.name}</span>
              <span
                class={`rounded px-1 text-[10px] ${
                  entry.is_git_repo ? 'bg-status-ok/15 text-status-ok' : 'text-trusty-text/30'
                }`}
              >
                {entry.is_git_repo ? 'git' : '—'}
              </span>
            </button>
            <button
              type="button"
              class="shrink-0 rounded border border-trusty-border px-1.5 py-0.5 text-trusty-text/70 hover:bg-trusty-border/20"
              onclick={() => selectEntry(entry)}
            >
              use
            </button>
          </li>
        {/each}
        {#if dirEntries.length === 0}
          <li class="text-trusty-text/40">no subdirectories</li>
        {/if}
      </ul>
    {:else}
      <p class="mt-1 text-xs text-trusty-text/50">loading…</p>
    {/if}

    <p class="mt-2 text-xs text-trusty-text/70">
      selected: <span class="font-mono">{bindingLabel(selectedProject)}</span>
      {#if selectedProject}
        <button type="button" class="ml-2 text-trusty-text/40 underline" onclick={clearSelection}>
          clear
        </button>
      {/if}
    </p>
  </div>

  <div class="mt-3">
    <label class="text-xs text-trusty-text/60" for="new-session-task">task</label>
    <textarea
      id="new-session-task"
      bind:value={task}
      rows="3"
      placeholder="Describe what this session should do…"
      class="mt-1 w-full rounded border border-trusty-border bg-trusty-surface/40 p-2 text-xs text-trusty-text"
    ></textarea>
  </div>

  <p
    class="mt-2 text-[11px] text-trusty-text/30"
    title="session.get_agents (GET /sessions/{'{'}id{'}'}/agents) requires an existing session — there is no pre-session roster route, so this form omits `agent` and the daemon applies its own default (DOC-39 §5.4)."
  >
    agent: daemon default — no pre-session roster endpoint exists yet
  </p>

  {#if submitError}
    <p class="mt-2 text-xs text-status-error">{submitError}</p>
  {/if}
  {#if successMessage}
    <p class="mt-2 text-xs text-status-ok">{successMessage}</p>
  {/if}

  <div class="mt-3">
    <button
      type="button"
      disabled={!canSubmitCreate(task, submitPhase)}
      onclick={submit}
      class="rounded border border-trusty-border px-3 py-1.5 text-xs font-medium text-trusty-text hover:bg-trusty-border/20 disabled:cursor-not-allowed disabled:opacity-40"
    >
      {submitPhase === 'submitting' ? 'creating…' : 'create session'}
    </button>
  </div>
</section>
