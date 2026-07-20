<script lang="ts">
  // Why: issue #3384 — Bob, after test-driving PR #3375's workstream-first
  // flow: "We shouldn't need to 'create' a workstream, it should be
  // inferred. Just pick a project and start working." This REMOVES the
  // explicit NEW WORKSTREAM creation step `NewWorkstreamForm.svelte` (issue
  // #3365, PR #3375) shipped — no more "new workstream" section header, no
  // "create workstream" button/ceremony. The flow collapses to exactly what
  // Bob describes: pick a project (`ProjectPickerModal`, optional — skip for
  // projectless) -> type the task -> go.
  //
  // **The underlying machinery is UNCHANGED — only the user-facing framing
  // is gone.** `submit()` below is byte-for-byte the same create -> run ->
  // activate orchestration PR #3375 shipped (workstream still mints
  // implicitly on first submit via `POST /workstreams`, named from the
  // interim `inferWorkstreamName` default — DOC-48 §9 Q1's prompt-derived
  // naming remains the open upgrade path, and matters MORE now that the
  // operator never types a name at all; still runs the first task via
  // `POST /tasks{workstream_id}` BEFORE activating via `POST
  // /workstreams/{id}/activate{force:true}`, deliberately LAST — see
  // `submit()`'s own doc, carried over verbatim from the code-critic PR
  // #3375 HIGH fix; still reuses `pendingWorkstreamId` on a
  // create-succeeded-but-run-failed retry rather than minting another). None
  // of that changed — component renamed `NewWorkstreamForm.svelte` ->
  // `StartWorkingForm.svelte` to match the new framing, `lib/new-workstream.ts`
  // (the pure logic module) keeps its name since the WORKSTREAM MACHINERY it
  // describes is exactly what is unchanged.
  //
  // **Renaming/closing/switching an EXISTING workstream is untouched** —
  // that stays in `WorkstreamSwitcher.svelte` (issue #3300/PR #3356) for
  // power users; this component only ever mints (or reuses a pending) NEW
  // one, never manages an existing one.
  //
  // **Projectless <-> unbound-session mapping (design call, carried over
  // from issue #3365).** The modal's "start chatting without a project"
  // option maps to a workstream whose first (and so far only) session has NO
  // `project` binding — DOC-39 §4.2's existing projectless state, unchanged
  // at the session layer. The UI never uses the word "session" or "unbound"
  // here — it stays workstream-neutral throughout.
  //
  // What: Markup drops the "new workstream" section header (this card now
  // reads "start working", echoing Bob's own phrase) and renames the submit
  // button/phase text from "create workstream"/"creating…" to "go"/
  // "starting…". The success message drops "workstream created" ceremony
  // language (and the session-id fragment it used to show) in favor of a
  // plain "started" — issue #3384 Scope B pushes session vocabulary out of
  // user-facing text project-wide, and a raw internal id was never
  // meaningful to the operator anyway; `WorkstreamActivity.svelte` (renamed
  // from `SessionMonitor.svelte`, same issue) is where live status now
  // lives. Two independent `$effect`s carried over unchanged in shape:
  // the submit-controller unmount-only teardown. There is still no network
  // call at mount — project selection happens inside `ProjectPickerModal`
  // (fetched only while that modal is open). Submit is gated by
  // `canSubmitCreate` (unchanged); `handleTaskKeydown`
  // (Enter-goes/Shift+Enter-newlines) is carried over unchanged from issue
  // #3132's fix.
  // Test: `StartWorkingForm.test.ts` covers the form's disabled/enabled
  // submit states, the no-double-submit guard, Enter-vs-Shift+Enter
  // keyboard behavior, the picker-modal wiring (open/select/clear), the
  // create -> run -> activate call ORDER (activation never fires before a
  // successful task-run), the workstream-id-reuse-on-retry path (no second
  // `POST /workstreams` after a task-run failure), and partial-failure
  // paths. `lib/new-workstream.test.ts` covers the pure
  // gating/body-construction/name-inference logic.
  import { apiBase } from '../lib/api-config';
  import {
    bindingLabel,
    buildCreateWorkstreamBody,
    buildRunTaskBody,
    canSubmitCreate,
    extractWorkstreamId,
    inferWorkstreamName,
    type ProjectSelection,
    type SubmitPhase,
  } from '../lib/new-workstream';
  import ProjectPickerModal from './ProjectPickerModal.svelte';

  let selectedProject = $state<ProjectSelection | null>(null);
  let pickerOpen = $state(false);
  let task = $state('');

  let submitPhase = $state<SubmitPhase>('idle');
  let submitError = $state<string | null>(null);
  let successMessage = $state<string | null>(null);

  // Set once `POST /workstreams` succeeds; cleared only once the WHOLE
  // sequence (through a successful task-run) completes. A `POST /tasks`
  // failure after a successful create leaves these set so the next
  // `submit()` call reuses the same workstream instead of minting another
  // (code-critic PR #3375 review, HIGH — see the module doc's note).
  let pendingWorkstreamId = $state<string | null>(null);
  let pendingWorkstreamName = $state<string | null>(null);

  let submitController: AbortController | null = null;

  function openPicker() {
    pickerOpen = true;
  }

  function closePicker() {
    pickerOpen = false;
  }

  function onProjectSelected(project: ProjectSelection | null) {
    selectedProject = project;
    pickerOpen = false;
  }

  function clearSelection() {
    selectedProject = null;
  }

  /**
   * Task-field `keydown` handler — issue #3132, carried over unchanged from
   * `CreateSessionForm.svelte`/`NewWorkstreamForm.svelte`.
   *
   * Why/What: plain `Enter` submits (goes), `Shift+Enter` inserts a newline
   * (the textarea's own default behavior).
   * Test: `StartWorkingForm.test.ts`.
   */
  function handleTaskKeydown(e: KeyboardEvent) {
    if (e.key !== 'Enter' || e.shiftKey || e.isComposing) return;
    e.preventDefault();
    void submit();
  }

  /**
   * Parse a REST error envelope's `error.message`, falling back to a bare
   * HTTP status when the body is missing/malformed.
   *
   * Why: two separate fetch calls in [`submit`] need the exact same
   * "extract a caller-actionable message from a non-OK response" step —
   * factored out once rather than repeated twice.
   */
  async function errorMessage(res: Response): Promise<string> {
    const body = (await res.json().catch(() => null)) as { error?: { message?: string } } | null;
    return body?.error?.message ?? `HTTP ${res.status}`;
  }

  /**
   * The create sequence: mint (or reuse) a workstream, run the first task
   * bound to it, and only THEN activate it. Byte-for-byte the same
   * orchestration `NewWorkstreamForm.svelte` (issue #3365/PR #3375) shipped
   * — issue #3384 changes only the surrounding UI framing, not this
   * function's logic.
   *
   * Why: DOC-48 has no single "create + run + activate" verb (§5.1's
   * surface is three independent REST routes), so this is client-side
   * orchestration. **Order is deliberate — create, then run, then activate
   * LAST** (code-critic PR #3375 review, HIGH): activation is `force: true`
   * (an explicit user action, so — unlike `WorkstreamSwitcher`'s
   * switch-between-EXISTING-workstreams flow, where `force: false` and a
   * surfaced `ActiveConflict` are correct because the target is ambiguous —
   * there is no ambiguity here) and therefore UNCONDITIONALLY deactivates
   * whatever the operator (or another attached client) had active. Firing
   * that BEFORE the task-run is known to succeed would steal activation away
   * for a workstream that might turn out to be empty forever (a task-run
   * failure leaves it stranded, active, with nothing bound). Deferring
   * activation to after a successful task-run costs nothing — `task.run`'s
   * `workstream_id` binds a session to a workstream by id alone, with no
   * activation precondition — and activation staying best-effort/non-fatal
   * is unchanged: a failed activation here (e.g. a race with another client)
   * does not undo the already-successful task-run; it is folded into the
   * success message instead.
   *
   * **Create-succeeded-but-run-failed keeps `pendingWorkstreamId` set**
   * (does not `return` through the cleared-state path) so the NEXT
   * `submit()` call reuses the same workstream id rather than minting a
   * second one. The error message names the workstream so the operator
   * knows it already exists.
   * Test: `StartWorkingForm.test.ts`.
   */
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

      let workstreamId = pendingWorkstreamId;
      let workstreamName = pendingWorkstreamName;
      if (!workstreamId) {
        workstreamName = inferWorkstreamName(selectedProject);
        const wsRes = await fetch(`${base}/workstreams`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(buildCreateWorkstreamBody(workstreamName)),
          signal: controller.signal,
        });
        if (controller.signal.aborted) return;
        if (wsRes.status !== 201) {
          submitError = await errorMessage(wsRes);
          return;
        }
        const wsCreated: unknown = await wsRes.json().catch(() => null);
        if (controller.signal.aborted) return;
        const newId = extractWorkstreamId(wsCreated);
        if (!newId) {
          submitError = 'workstream created but the daemon did not return an id';
          return;
        }
        workstreamId = newId;
        pendingWorkstreamId = newId;
        pendingWorkstreamName = workstreamName;
      }

      const body = buildRunTaskBody(task, selectedProject, workstreamId);
      const res = await fetch(`${base}/tasks`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
        signal: controller.signal,
      });
      if (controller.signal.aborted) return;

      if (res.status !== 202) {
        // `pendingWorkstreamId`/`pendingWorkstreamName` deliberately stay
        // set here — the workstream exists; a retry must reuse it, not
        // mint another (code-critic PR #3375 review, HIGH).
        const daemonMessage = await errorMessage(res);
        submitError = `${daemonMessage} — workstream "${workstreamName}" was created but not activated; it will be reused if you retry.`;
        return;
      }

      // Task run succeeded — NOW activate (best-effort, deliberately last;
      // see the doc above for why this is not the second step).
      let activationWarning: string | null = null;
      try {
        const actRes = await fetch(`${base}/workstreams/${workstreamId}/activate`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ force: true }),
          signal: controller.signal,
        });
        if (controller.signal.aborted) return;
        if (!actRes.ok) {
          activationWarning = `could not activate the new workstream (HTTP ${actRes.status})`;
        }
      } catch (e) {
        if (controller.signal.aborted) return;
        activationWarning = e instanceof Error ? e.message : String(e);
      }

      // The response body (`{session_id, status, mode, binding}`) carries no
      // information this form still displays — issue #3384 drops the
      // "workstream created — session <id>" ceremony message in favor of a
      // plain "started"; `WorkstreamActivity.svelte` is where live status
      // now lives. The body is intentionally left unconsumed.
      successMessage = activationWarning ? `started (${activationWarning})` : 'started';
      task = '';
      selectedProject = null;
      pendingWorkstreamId = null;
      pendingWorkstreamName = null;
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
    // Unmount-only teardown for the submit request — creation is
    // user-triggered, not polled, so this effect has no reactive deps.
    return () => {
      submitController?.abort();
    };
  });
</script>

<section class="mt-4 rounded border-1.5 border-trusty-border bg-trusty-card">
  <div class="border-b border-trusty-border bg-trusty-raised px-4 py-2.5">
    <h2 class="font-display text-xs font-bold uppercase tracking-wide text-trusty-text">
      start working
    </h2>
  </div>

  <div class="p-4">
    <div>
      <p class="font-mono text-[10px] font-semibold uppercase tracking-wide text-trusty-text-muted">
        project
      </p>
      <p class="mt-1 text-xs text-trusty-text-secondary">
        selected: <span class="font-mono">{bindingLabel(selectedProject)}</span>
      </p>
      <div class="mt-1.5 flex items-center gap-2">
        <button
          type="button"
          onclick={openPicker}
          class="rounded-sm border-1.5 border-trusty-border-strong bg-trusty-card px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-trusty-text-secondary hover:border-trusty-primary hover:text-trusty-primary"
        >
          choose project
        </button>
        {#if selectedProject}
          <button
            type="button"
            class="font-mono text-[11px] uppercase tracking-wide text-trusty-text-muted underline hover:text-trusty-primary"
            onclick={clearSelection}
          >
            clear
          </button>
        {/if}
      </div>
    </div>

    <div class="mt-3">
      <label
        class="font-mono text-[10px] font-semibold uppercase tracking-wide text-trusty-text-muted"
        for="start-working-task"
      >
        task
      </label>
      <textarea
        id="start-working-task"
        bind:value={task}
        onkeydown={handleTaskKeydown}
        rows="3"
        placeholder="Describe what you want to work on… (Enter to go, Shift+Enter for a new line)"
        class="mt-1 w-full rounded-sm border-1.5 border-trusty-border-strong bg-trusty-card p-2 text-xs text-trusty-text"
      ></textarea>
    </div>

    <p
      class="mt-2 text-[11px] text-trusty-text-muted"
      title="session.get_agents (GET /sessions/{'{'}id{'}'}/agents) requires an existing session — there is no pre-task AGENT roster route (distinct from this ticket's own project roster), so this form omits `agent_name` and the daemon applies its own default (DOC-39 §5.4)."
    >
      agent: daemon default — no pre-task agent roster endpoint exists yet
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
        {submitPhase === 'submitting' ? 'starting…' : 'go'}
      </button>
    </div>
  </div>
</section>

<ProjectPickerModal open={pickerOpen} onSelect={onProjectSelected} onClose={closePicker} />
