<script lang="ts">
  // Why: issue #3365 — Bob's product direction on the live Foundry screen:
  // "Let's call it a new workstream, and users pick a project or just start
  // chatting… let's focus on 'workstreams' as the primary connection path,
  // not sessions — since we have infinite sessions, we shouldn't need
  // them." This REPLACES `CreateSessionForm`'s session-first entry (inline
  // 7a folder picker + `POST /tasks`) with a workstream-first one: the
  // project picker moves into `ProjectPickerModal` (a roster, not a
  // browse-and-descend tree), and submitting now mints a WORKSTREAM first
  // (`POST /workstreams`), runs the first task bound to it (`POST /tasks`
  // with `workstream_id`, PR #3354's binding support, previously unused by
  // any GUI caller), and only THEN activates it (`POST
  // /workstreams/{id}/activate` — see the `submit` doc below for why this
  // is deliberately the LAST step, not the second one) — "sessions are
  // created under the hood," per the issue. `lib/new-workstream.ts`
  // (renamed from `lib/create-session.ts`) carries every piece of pure
  // gating/body-construction logic this component depends on.
  //
  // **Create -> run -> activate, not create -> activate -> run
  // (code-critic PR #3375 review, HIGH).** The original ordering activated
  // the brand-new workstream (`force: true`, unconditionally deactivating
  // whatever the operator — or another attached client — had active)
  // BEFORE knowing whether `POST /tasks` would even succeed. A task-run
  // failure after that point left an EMPTY, now-active workstream stranded
  // in place of whatever was active before, with no restore and no mention
  // of it in the error text. Activation was already documented as
  // best-effort/non-gating (task.run's `workstream_id` binds by id alone,
  // no activation precondition) — so deferring it to AFTER a successful
  // task-run costs nothing and removes both the activation-steal-on-failure
  // and the orphan-goes-active problems in one move.
  //
  // **Retry reuses the already-minted workstream (same review).** If
  // `POST /workstreams` succeeds but `POST /tasks` then fails, the created
  // (but not yet activated) workstream id is kept in `pendingWorkstreamId`
  // rather than discarded — a retry binds the SAME workstream instead of
  // minting a second one. Only a failure at the CREATE step itself (no id
  // was ever minted) causes the next attempt to mint fresh. This prevents
  // each retry-after-task-failure from compounding into another abandoned
  // workstream.
  //
  // **Projectless <-> unbound-session mapping (design call, documented per
  // the issue).** The modal's "start chatting without a project" option
  // maps to a workstream whose first (and so far only) session has NO
  // `project` binding — DOC-39 §4.2's existing projectless state, unchanged
  // at the session layer. The UI never uses the word "session" or
  // "unbound" here; it stays workstream-neutral throughout, matching the
  // issue's explicit instruction ("UI copy stays workstream-neutral").
  //
  // What: Two independent `$effect`s carried over unchanged in shape from
  // the prior form: the submit-controller unmount-only teardown. The
  // mount-time `GET /fs` picker fetch is GONE — there is no longer any
  // network call at mount, since project selection now happens inside
  // `ProjectPickerModal` (fetched only while that modal is open). Submit is
  // gated by `canSubmitCreate` (unchanged); `handleTaskKeydown`
  // (Enter-submits/Shift+Enter-newlines) is carried over unchanged from
  // issue #3132's fix.
  // Test: `NewWorkstreamForm.test.ts` covers the form's disabled/enabled
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
    extractTaskSessionId,
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
  // (code-critic PR #3375 review, HIGH — see the module doc's "Retry
  // reuses…" note).
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
   * `CreateSessionForm.svelte`.
   *
   * Why/What: see the prior component's identical handler — plain `Enter`
   * submits, `Shift+Enter` inserts a newline (the textarea's own default
   * behavior).
   * Test: `NewWorkstreamForm.test.ts`.
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
   * Why: three separate fetch calls in [`submit`] need the exact same
   * "extract a caller-actionable message from a non-OK response" step —
   * factored out once rather than repeated three times.
   */
  async function errorMessage(res: Response): Promise<string> {
    const body = (await res.json().catch(() => null)) as { error?: { message?: string } } | null;
    return body?.error?.message ?? `HTTP ${res.status}`;
  }

  /**
   * The create sequence: mint (or reuse) a workstream, run the first task
   * bound to it, and only THEN activate it.
   *
   * Why: DOC-48 has no single "create + run + activate" verb (§5.1's
   * surface is three independent REST routes), so this is the client-side
   * orchestration issue #3365 asks for. **Order is deliberate — create,
   * then run, then activate LAST** (code-critic PR #3375 review, HIGH):
   * activation is `force: true` (an explicit user action, so — unlike
   * `WorkstreamSwitcher`'s switch-between-EXISTING-workstreams flow, where
   * `force: false` and a surfaced `ActiveConflict` are correct because the
   * target is ambiguous — there is no ambiguity here) and therefore
   * UNCONDITIONALLY deactivates whatever the operator (or another attached
   * client) had active. Firing that BEFORE the task-run is known to
   * succeed would steal activation away for a workstream that might turn
   * out to be empty forever (a task-run failure leaves it stranded, active,
   * with nothing bound). Deferring activation to after a successful
   * task-run costs nothing — `task.run`'s `workstream_id` binds a session
   * to a workstream by id alone, with no activation precondition — and
   * activation staying best-effort/non-fatal is unchanged: a failed
   * activation here (e.g. a race with another client) does not undo the
   * already-successful task-run; it is folded into the success message
   * instead.
   *
   * **Create-succeeded-but-run-failed keeps `pendingWorkstreamId` set**
   * (does not `return` through the cleared-state path) so the NEXT
   * `submit()` call reuses the same workstream id rather than minting a
   * second one — see the module doc's "Retry reuses…" note. The error
   * message names the workstream so the operator knows it already exists.
   * Test: `NewWorkstreamForm.test.ts`.
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

      const created: unknown = await res.json().catch(() => null);
      if (controller.signal.aborted) return;
      const sessionId = extractTaskSessionId(created);
      const base_message = sessionId
        ? `workstream created — session ${sessionId.slice(0, 8)}`
        : 'workstream created';
      successMessage = activationWarning ? `${base_message} (${activationWarning})` : base_message;
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
      new workstream
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
        for="new-workstream-task"
      >
        task
      </label>
      <textarea
        id="new-workstream-task"
        bind:value={task}
        onkeydown={handleTaskKeydown}
        rows="3"
        placeholder="Describe what this workstream should do… (Enter to submit, Shift+Enter for a new line)"
        class="mt-1 w-full rounded-sm border-1.5 border-trusty-border-strong bg-trusty-card p-2 text-xs text-trusty-text"
      ></textarea>
    </div>

    <p
      class="mt-2 text-[11px] text-trusty-text-muted"
      title="session.get_agents (GET /sessions/{'{'}id{'}'}/agents) requires an existing session — there is no pre-session AGENT roster route (distinct from this ticket's own project roster), so this form omits `agent_name` and the daemon applies its own default (DOC-39 §5.4)."
    >
      agent: daemon default — no pre-session agent roster endpoint exists yet
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
        {submitPhase === 'submitting' ? 'creating…' : 'create workstream'}
      </button>
    </div>
  </div>
</section>

<ProjectPickerModal open={pickerOpen} onSelect={onProjectSelected} onClose={closePicker} />
