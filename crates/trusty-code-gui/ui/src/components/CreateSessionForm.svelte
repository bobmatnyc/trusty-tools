<script lang="ts">
  // Why: DOC-39's biggest gap called out for this slice — the GUI could only
  // observe and cancel a session, never create one. This is the create+
  // prompt flow: the 7a folder picker (§4.2.1) plus the minimal task-input
  // form needed to call `POST /tasks` (`task.run`, #2983 Slice 4,
  // `crate::serve::rest::tasks`) — the one-shot "mint-or-reuse a session AND
  // start executing" entry point DOC-39 §7A/AC-21 names. This form used to
  // call `POST /sessions` (`session.create`), which only ever minted an inert
  // session record with no agent loop behind it (issue #3177) — every
  // GUI-created session sat doing nothing. `POST /tasks` fixes that: typing a
  // task and hitting submit now actually runs it, with the per-call `project`
  // binding (PR #3189) carried the same way `POST /sessions` already
  // forwarded it. Two daemon routes cover the whole slice — no Tauri command,
  // no native dialog (barred by §2.1 C-4, see `lib/create-session.ts`'s
  // module doc), identical behavior in web and Tauri per C-3.
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
  // supported, not treated as an error state. The row's `use` button sits
  // directly adjacent to the directory name (issue #3134, Bob's smoke-test
  // feedback: the prior `flex-1`/`justify-between` layout stretched the
  // name button to fill the row and stranded `use` at the far right, wide
  // gap included) — see the row markup's inline comment.
  //
  // What: Two independent `$effect`s, same `AbortController`-per-lifetime
  // shape `SessionMonitor.svelte`/`StatusBar.svelte` already establish: one
  // re-fetches `GET /fs?path=..` whenever `browsePath` changes (aborting the
  // previous in-flight listing fetch — a fast double-click through several
  // directories must not let a stale response win the race), the other only
  // aborts `submitController` on unmount (`POST /tasks` is user-triggered,
  // not polled, so it needs no interval). The prior listing stays on screen
  // while a new one loads (`listingPhase === 'loading'` renders a small
  // inline cue rather than blanking the rows) to avoid picker flicker while
  // navigating. Submit is gated by `canSubmitCreate` (non-empty task, not
  // already in flight — `lib/create-session.ts`), and the button's `disabled`
  // binding is the only double-submit guard needed since there is no
  // separate confirm step for creation (unlike `SessionMonitor`'s cancel).
  // A successful `202` clears `task`/`selectedProject` and shows the new
  // session's id when the body carries one (a generic "session created"
  // otherwise — the 202 status, not the body, is authoritative). Both
  // response bodies pass runtime shape guards (`isDirListing` /
  // `extractTaskSessionId`'s internal `isTaskRunResponse` check) before
  // touching reactive state, so a shape-invalid
  // 200/202 degrades to an inline message instead of throwing out of this
  // unconditionally-mounted component and crashing the shell. A `400`
  // (empty task, invalid `project`, or a `project` mismatching an existing
  // session's own binding — #3178/PR #3189) surfaces via the daemon's
  // `error.message` unchanged, the same generic 4xx/5xx path this form
  // already had.
  // The session itself becomes visible through the existing
  // `GET /sessions` pollers (`StatusBar`/`SessionMonitor`'s `pickActiveSession`)
  // on their next tick — this component does not need its own poll.
  // Enter-to-submit (issue #3132, Bob's smoke-test feedback: the form had
  // no keyboard submit path — click was the only way in) is handled by
  // `handleTaskKeydown` on the task `<textarea>`: plain `Enter` submits,
  // `Shift+Enter` inserts a newline (the textarea's own default behavior,
  // untouched) — see that function's own doc comment for the full
  // rationale and why no separate double-submit guard was needed.
  //
  // Test: `create-session.test.ts` covers the pure gating/body-construction
  // logic; `CreateSessionForm.test.ts` covers the form's disabled/enabled
  // submit states, the no-double-submit guard (click AND rapid Enter),
  // Enter-vs-Shift+Enter keyboard behavior, and picker navigation.
  import { apiBase } from '../lib/api-config';
  import {
    bindingLabel,
    buildRunTaskBody,
    canSubmitCreate,
    describeFsError,
    extractTaskSessionId,
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

  /**
   * Task-field `keydown` handler — issue #3132.
   *
   * Why: the form had no keyboard submit path at all — clicking "create
   * session" was the only way in, which Bob's smoke test flagged as wrong
   * UX (`docs/specs/trusty-code-harness-ui.md` has no submit-key
   * convention to defer to, so this follows the universal textarea
   * pattern: Enter submits, Shift+Enter inserts a newline). A plain
   * `<textarea>`'s native behavior is to insert a newline on every Enter
   * (never auto-submit, unlike a single-line `<input>` in a `<form>`), so
   * Shift+Enter needs no handling at all here — only the plain-Enter case
   * needs to be intercepted and redirected to `submit()`.
   * What: `Enter` without `Shift` (and not mid-IME-composition) calls
   * `preventDefault()` — so no newline is inserted — then defers to the
   * existing `submit()`, which already re-checks `canSubmitCreate` at its
   * top; this handler adds no separate guard, so the no-double-submit
   * semantics are identical to the button's `disabled` binding. `Shift+Enter`
   * (or any other key) falls through untouched, letting the textarea's
   * default newline-insertion behavior apply.
   * Test: `CreateSessionForm.test.ts` — Enter submits, Shift+Enter does not
   * (and is never prevented), and a second rapid Enter while the first
   * submit is in flight produces no second `POST`.
   */
  function handleTaskKeydown(e: KeyboardEvent) {
    if (e.key !== 'Enter' || e.shiftKey || e.isComposing) return;
    e.preventDefault();
    void submit();
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

      const body = buildRunTaskBody(task, selectedProject);
      const res = await fetch(`${base}/tasks`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
        signal: controller.signal,
      });
      if (controller.signal.aborted) return;

      if (res.status !== 202) {
        // Covers the daemon's existing 4xx/5xx envelope, including the
        // per-call project mismatch 400 (#3178, PR #3189) — its message is
        // already caller-actionable, so it needs no special-casing here.
        const errBody = (await res.json().catch(() => null)) as {
          error?: { message?: string };
        } | null;
        submitError = errBody?.error?.message ?? `HTTP ${res.status}`;
        return;
      }

      // The 202 status is authoritative — task.run was accepted even if the
      // body is missing/malformed (same guard rationale as `isDirListing`),
      // so a bad body degrades the message to a generic one, never an error.
      const created: unknown = await res.json().catch(() => null);
      if (controller.signal.aborted) return;
      const id = extractTaskSessionId(created);
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

<section class="mt-4 rounded border-1.5 border-trusty-border bg-trusty-card">
  <div class="border-b border-trusty-border bg-trusty-raised px-4 py-2.5">
    <h2 class="font-display text-xs font-bold uppercase tracking-wide text-trusty-text">
      new session
    </h2>
  </div>

  <div class="p-4">
    <div>
      <p class="font-mono text-[10px] font-semibold uppercase tracking-wide text-trusty-text-muted">
        project folder
      </p>

      {#if listingPhase === 'error' && !listing}
        <p class="mt-1 text-xs text-status-error">
          daemon unreachable{listingError ? ` — ${listingError}` : ''}
        </p>
      {:else if listing}
        <div class="mt-1 flex items-center gap-2 text-xs text-trusty-text-secondary">
          <button
            type="button"
            disabled={!listing.parent}
            onclick={goUp}
            class="rounded-sm border-1.5 border-trusty-border-strong bg-trusty-card px-1.5 py-0.5 font-mono text-[11px] uppercase tracking-wide disabled:cursor-not-allowed disabled:opacity-40"
          >
            ↑ up
          </button>
          <span class="truncate font-mono" title={listing.path}>{listing.display_path}</span>
          {#if listingPhase === 'loading'}
            <span class="text-trusty-text-muted">loading…</span>
          {/if}
        </div>

        <ul class="mt-2 max-h-40 space-y-1 overflow-y-auto text-xs">
          {#each dirEntries as entry (entry.path)}
            <!-- issue #3134: the "use" (picker) button sits DIRECTLY adjacent
                 to the directory name it acts on. The prior markup put
                 `flex-1` on the name button and `justify-between` on the
                 row, which stretched the name button to fill the row and
                 shoved "use" to the row's far right — for a short name that
                 left a wide, disorienting gap between the name and the
                 control that binds it. `min-w-0 max-w-[65%]` still bounds
                 the name button's width so `truncate` keeps working on long
                 names, but without `flex-1`/`justify-between` the row's
                 default `flex items-center` packs both buttons together at
                 the left, so "use" always immediately follows the name
                 (any leftover row width is simply unused space at the end,
                 not a gap in the middle). -->
            <li class="flex items-center gap-2 rounded-sm px-1.5 py-1 hover:bg-trusty-raised">
              <button
                type="button"
                class="flex min-w-0 max-w-[65%] items-center gap-1.5 truncate text-left text-trusty-text"
                onclick={() => openEntry(entry)}
              >
                <span class="truncate">{entry.name}</span>
                <span
                  class={`rounded-sm px-1 font-mono text-[10px] uppercase tracking-wide ${
                    entry.is_git_repo ? 'bg-status-ok/15 text-status-ok' : 'text-trusty-text-muted'
                  }`}
                >
                  {entry.is_git_repo ? 'git' : '—'}
                </span>
              </button>
              <button
                type="button"
                class="shrink-0 rounded-sm border-1.5 border-trusty-border-strong bg-trusty-card px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-trusty-text-secondary hover:border-trusty-primary hover:text-trusty-primary"
                onclick={() => selectEntry(entry)}
              >
                use
              </button>
            </li>
          {/each}
          {#if dirEntries.length === 0}
            <li class="text-trusty-text-muted">no subdirectories</li>
          {/if}
        </ul>
      {:else}
        <p class="mt-1 text-xs text-trusty-text-muted">loading…</p>
      {/if}

      <p class="mt-2 text-xs text-trusty-text-secondary">
        selected: <span class="font-mono">{bindingLabel(selectedProject)}</span>
        {#if selectedProject}
          <button
            type="button"
            class="ml-2 font-mono text-[11px] uppercase tracking-wide text-trusty-text-muted underline hover:text-trusty-primary"
            onclick={clearSelection}
          >
            clear
          </button>
        {/if}
      </p>
    </div>

    <div class="mt-3">
      <label
        class="font-mono text-[10px] font-semibold uppercase tracking-wide text-trusty-text-muted"
        for="new-session-task"
      >
        task
      </label>
      <textarea
        id="new-session-task"
        bind:value={task}
        onkeydown={handleTaskKeydown}
        rows="3"
        placeholder="Describe what this session should do… (Enter to submit, Shift+Enter for a new line)"
        class="mt-1 w-full rounded-sm border-1.5 border-trusty-border-strong bg-trusty-card p-2 text-xs text-trusty-text"
      ></textarea>
    </div>

    <p
      class="mt-2 text-[11px] text-trusty-text-muted"
      title="session.get_agents (GET /sessions/{'{'}id{'}'}/agents) requires an existing session — there is no pre-session roster route, so this form omits `agent_name` and the daemon applies its own default (DOC-39 §5.4)."
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
        class="rounded-sm border-1.5 border-trusty-primary-hover bg-trusty-primary px-3 py-1.5 font-mono text-xs font-semibold uppercase tracking-wide text-trusty-text-inverse hover:bg-trusty-primary-hover disabled:cursor-not-allowed disabled:border-trusty-border disabled:bg-trusty-raised disabled:text-trusty-text-muted"
      >
        {submitPhase === 'submitting' ? 'creating…' : 'create session'}
      </button>
    </div>
  </div>
</section>
