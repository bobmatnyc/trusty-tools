<script lang="ts">
  // Why: issue #3446 — Bob, after test-driving the implicit-workstream flow:
  // "'Start Working' should be at the bottom of the pane, and the workstream
  // activity above it. Refactor the main chat pane similar to a TUI. Chat at
  // the bottom, enter as a button to the right (or enter), and the stream
  // builds up." This component is now a PERSISTENT BOTTOM-DOCKED INPUT BAR
  // (`WorkstreamTab.svelte` stacks it below `WorkstreamActivity`, which fills
  // the rest of the pane and scrolls as the chat stream — see that
  // component's own docs) rather than a titled card — no more "start
  // working" section header; the textarea + send button ARE the whole
  // surface now, chat-composer-style.
  //
  // **All prior semantics are preserved — layout/interaction shell only.**
  // Project picker access (`ProjectPickerModal`), implicit workstream mint
  // (create -> run -> activate, deliberately in that order — code-critic PR
  // #3375 HIGH fix), `pendingWorkstreamId` retry-reuse on a create-succeeded-
  // but-run-failed attempt, and the projectless path are all byte-for-byte
  // the same mechanics issues #3365/#3384/#3392 shipped.
  //
  // **Chat continuation is the one behavioral addition (issue #3446 Scope
  // 3).** Investigated what the daemon actually supports for a "follow-up
  // turn" vs. a brand-new task before picking an implementation:
  // `SessionRegistry::begin_execution` (`crates/trusty-code/src/session/
  // registry.rs`) allows a `Finished` session to be RESUMED back to
  // `Running` by a second `task.run{session_id}` call — a genuine follow-up
  // TURN appended to the SAME session's transcript, not a new session. Only
  // `Cancelled`/`Failed`/`DeadlineExceeded` sessions are permanently
  // terminal (`begin_execution` rejects them outright), and a session with
  // an execution already in flight rejects a second overlapping call. This
  // is the HONEST implementation: `lastSessionId` (set once a task-run
  // succeeds, mint or continuation) is tried FIRST via `session_id` on every
  // subsequent submit — genuinely resuming the same session/transcript when
  // the daemon allows it.
  //
  // **A rejected reuse is disambiguated before any fallback (code-critic PR
  // #3460 review, HIGH 1).** `begin_execution` returns the SAME
  // invalid-argument rejection for "session is terminal" and "session
  // already has a task running" — and since `task.run` is 202-then-
  // background, a quick follow-up while the first task still runs is
  // ROUTINE, not exotic. Blindly treating every rejection as "reuse failed,
  // mint a fresh session under the same workstream" would therefore fork a
  // SECOND concurrent session for the routine case: the still-running old
  // session becomes an invisible orphan (this pane shows only the newest
  // bound session), uncancelable from the UI, doubling LLM spend, with two
  // agents possibly writing the same tree. So on rejection the form probes
  // `GET /sessions/{id}` first: a `running` status BLOCKS the submit with a
  // visible "still running" message (wait, or cancel from the activity
  // pane) instead of forking; a terminal status (or a 404 — the session
  // vanished) falls back to minting a FRESH session under the SAME
  // already-active workstream (`continuationWorkstreamId`, explicit
  // `workstream_id`, no second `POST /workstreams`) with a VISIBLE notice
  // that a fresh session was started; an unverifiable probe (network
  // failure, malformed body) refuses to mint blind and surfaces the error —
  // never an orphan by default.
  //
  // **Continuation also re-targets when the daemon's ACTIVE workstream
  // changes for any reason (code-critic PR #3460 review, HIGH 2).** The
  // header's `WorkstreamSwitcher` switches workstreams without touching the
  // selected-project store, so a project-change-only reset left the
  // sequence "converse in A -> switch to B -> type" silently appending to
  // the now-hidden workstream A. Both workstream pollers
  // (`WorkstreamActivity`/`WorkstreamSwitcher`) now publish the daemon's
  // RESOLVED active id to `lib/active-workstream.svelte.ts` (one shared
  // resolution rule — see that module's docs); the `$effect` below watches
  // it and, on any genuine change to an id this conversation isn't already
  // targeting, drops `lastSessionId` and ADOPTS the new id as
  // `continuationWorkstreamId` — so the next submit runs a fresh session
  // under the workstream the operator just switched TO (ambient targeting,
  // Bob's "subsequent submits target the ACTIVE workstream"), rather than
  // continuing hidden-A or minting a surprise third workstream. Adoption
  // happens only on an OBSERVED change, never on mount: the first message
  // of a fresh form still mints implicitly (issue #3384's flow, unchanged).
  //
  // **Project-selection persistence + chat continuation are ONE state
  // model (issue #3447 bug 1, solved together per the coordinator's own
  // framing).** Root cause of "project selection doesn't stick": the PRIOR
  // `submit()` unconditionally reset `selectedProject = null` at the end of
  // every successful call — so the very next glance at the form (never mind
  // the next submit) already read back "projectless." Fixed by (a) moving
  // the selection into `lib/selected-project.svelte.ts`, a shared
  // cross-module store `WorkstreamRail`'s new Projects section (issue #3447
  // bug 2) ALSO reads/writes, and (b) simply never resetting it on success
  // anymore — a picked project is the active binding until the operator
  // explicitly changes it (a different pick, or "clear"). Changing the
  // selection is exactly the signal that also resets chat continuation: a
  // session's project binding is immutable once set
  // (`task::protocol::task_run`'s docs), so switching projects mid-
  // conversation can only honestly mean "start a new workstream on the next
  // submit" — the `$effect` below watches the shared selection's `path` and
  // clears `lastSessionId`/`continuationWorkstreamId` (and the mint-retry
  // state) whenever it changes.
  //
  // What: Markup keeps the same project-selection row (now reading the
  // shared store) and the same textarea, restyled as a docked bar: the send
  // button sits to the textarea's right (issue #3446's literal ask) rather
  // than below it, and the "new workstream"/"start working" card header is
  // gone. `handleTaskKeydown` (Enter-sends/Shift+Enter-newlines) is carried
  // over unchanged from issue #3132's fix.
  // Test: `StartWorkingForm.test.ts` covers the docked-bar layout (no
  // heading, send button to the textarea's right), Enter-vs-Shift+Enter,
  // the no-double-submit guard, the picker-modal wiring, the create -> run
  // -> activate call ORDER, the workstream-id-reuse-on-retry path, project-
  // selection PERSISTENCE across a successful submit (issue #3447 bug 1),
  // and the second-submit chat-continuation call sequence (issue #3446 — no
  // second `POST /workstreams`, `session_id` reuse tried first, project
  // carried forward). `lib/new-workstream.test.ts` covers the pure
  // gating/body-construction/name-inference logic.
  import { activeWorkstreamState } from '../lib/active-workstream.svelte';
  import { apiBase } from '../lib/api-config';
  import {
    bindingLabel,
    buildCreateWorkstreamBody,
    buildRunTaskBody,
    canSubmitCreate,
    extractTaskSessionId,
    extractWorkstreamId,
    inferWorkstreamName,
    type SubmitPhase,
  } from '../lib/new-workstream';
  import { clearPendingWorkstream, setPendingWorkstream } from '../lib/pending-workstream.svelte';
  import { selectedProjectState, selectProject } from '../lib/selected-project.svelte';
  import { fetchAgentRoster, type AgentCatalogEntry } from '../lib/agent-roster';
  import ProjectPickerModal from './ProjectPickerModal.svelte';

  /** Delay between the two `activateWithRetry` attempts — see that
   * function's own doc for why a single retry, not unbounded. */
  const ACTIVATE_RETRY_DELAY_MS = 250;

  let pickerOpen = $state(false);
  let task = $state('');

  // Issue #3449 closes the "no pre-task agent roster endpoint" gap this
  // form previously carried as a standing note (`GET /agents`,
  // `lib/agent-roster.ts`). Fetched once on mount, best-effort — a failure
  // just leaves the selector at its "daemon default" option, exactly the
  // prior (only) behavior, rather than blocking the form.
  let agentRoster = $state<AgentCatalogEntry[]>([]);
  let selectedAgent = $state('');

  let submitPhase = $state<SubmitPhase>('idle');
  let submitError = $state<string | null>(null);
  let successMessage = $state<string | null>(null);

  // Mint-retry state (issues #3365/#3392) — set once `POST /workstreams`
  // succeeds; cleared only once the WHOLE first-message sequence (through a
  // successful task-run) completes. A `POST /tasks` failure after a
  // successful create leaves these set so the next `submit()` call reuses
  // the same workstream instead of minting another.
  let pendingWorkstreamId = $state<string | null>(null);
  let pendingWorkstreamName = $state<string | null>(null);

  // Chat-continuation state (issue #3446) — set once the FIRST task-run in
  // this conversation succeeds (mint path) and updated on every subsequent
  // successful submit (continuation path); NEVER cleared on success — only
  // when the selected project changes (reset to null: next submit mints)
  // or the daemon's active workstream changes (re-targeted to the new
  // active id — code-critic PR #3460 review, HIGH 2); see the two
  // `$effect`s below.
  let lastSessionId = $state<string | null>(null);
  let continuationWorkstreamId = $state<string | null>(null);

  let submitController: AbortController | null = null;

  function openPicker() {
    pickerOpen = true;
  }

  function closePicker() {
    pickerOpen = false;
  }

  function onProjectSelected(project: Parameters<typeof selectProject>[0]) {
    selectProject(project);
    pickerOpen = false;
  }

  function clearSelection() {
    selectProject(null);
  }

  /**
   * Reset chat-continuation state whenever the SHARED selected-project
   * store changes (issue #3446 + #3447 bug 1, one state model).
   *
   * Why: a session's project binding is immutable once set
   * (`task::protocol::task_run`'s docs — a per-call `project` may only
   * RESTATE a reused session's own binding, never change it), so if the
   * operator picks a different project (via this form's own modal OR
   * `WorkstreamRail`'s new Projects section — both write to the same
   * shared store) or clears the selection mid-conversation, continuing the
   * OLD session/workstream would either silently fail the immutability
   * check or, worse, keep running against a project the operator just
   * moved away from. The only honest behavior is: a project change starts
   * fresh on the next submit.
   * What: tracks the previous selection's `path` (or `null`) across runs;
   * on a genuine change (not the initial mount run), clears BOTH the chat-
   * continuation state and the mint-retry state (a stale retry-reuse mint
   * for the OLD project would be equally wrong) and the shared
   * `pendingWorkstream` fallback marker (also now stale).
   * Test: `StartWorkingForm.test.ts`.
   */
  let previousProjectPath: string | null | undefined = undefined;
  $effect(() => {
    const currentPath = selectedProjectState.project?.path ?? null;
    if (previousProjectPath !== undefined && previousProjectPath !== currentPath) {
      lastSessionId = null;
      continuationWorkstreamId = null;
      pendingWorkstreamId = null;
      pendingWorkstreamName = null;
      clearPendingWorkstream();
    }
    previousProjectPath = currentPath;
  });

  /**
   * Re-target chat continuation whenever the daemon's ACTIVE workstream
   * changes for any reason (code-critic PR #3460 review, HIGH 2).
   *
   * Why: see the module doc's HIGH-2 section — `WorkstreamSwitcher` never
   * touches the selected-project store, so without this a switch left the
   * input bar targeting the previous workstream's session.
   * What: watches the shared `activeWorkstreamState.id` (published by both
   * workstream pollers, one shared resolution rule — `lib/
   * active-workstream.svelte.ts`). On an OBSERVED change (never the mount-
   * time initial read — the first message of a fresh form still mints
   * implicitly, issue #3384's flow) to an id this conversation is not
   * already targeting: drop `lastSessionId`, ADOPT the new id as
   * `continuationWorkstreamId` (`null` when nothing is active — the next
   * submit then mints), and clear the mint-retry ids (a retry-reuse mint
   * held for the OLD workstream would be equally stale). Deliberately does
   * NOT clear `pending-workstream.svelte.ts`'s marker: unlike the project-
   * change effect above (where the operator is abandoning the conversation
   * wholesale), the resolved id arriving here may itself COME from that
   * marker (the activation-failed fallback), and the marker's owner
   * (`WorkstreamActivity`) still needs it. The "already targeting" guard
   * also makes this a no-op when the store merely catches up to a
   * workstream this form itself just minted and activated.
   * Test: `StartWorkingForm.test.ts`.
   */
  let previousActiveWorkstreamId: string | null | undefined = undefined;
  $effect(() => {
    const currentId = activeWorkstreamState.id;
    if (
      previousActiveWorkstreamId !== undefined &&
      previousActiveWorkstreamId !== currentId &&
      currentId !== continuationWorkstreamId
    ) {
      lastSessionId = null;
      continuationWorkstreamId = currentId;
      pendingWorkstreamId = null;
      pendingWorkstreamName = null;
    }
    previousActiveWorkstreamId = currentId;
  });

  /**
   * Task-field `keydown` handler — issue #3132, carried over unchanged from
   * `CreateSessionForm.svelte`/`NewWorkstreamForm.svelte`.
   *
   * Why/What: plain `Enter` sends, `Shift+Enter` inserts a newline (the
   * textarea's own default behavior).
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
   * Why: every fetch call in [`submit`] needs the exact same "extract a
   * caller-actionable message from a non-OK response" step — factored out
   * once rather than repeated at each call site.
   */
  async function errorMessage(res: Response): Promise<string> {
    const body = (await res.json().catch(() => null)) as { error?: { message?: string } } | null;
    return body?.error?.message ?? `HTTP ${res.status}`;
  }

  /**
   * Activate `workstreamId`, retrying ONCE on failure before giving up
   * (code-critic PR #3392 review, MEDIUM).
   *
   * Why: most activation failures at this point are transient — a momentary
   * race with another client's own activate call, a blip — so a single
   * retry silently closes the common case before ever falling back to
   * `pending-workstream.svelte.ts`'s cross-module store or surfacing a
   * warning to the operator. Deliberately bounded at one retry, not
   * unbounded: a genuinely broken daemon connection must still surface
   * promptly, not hang the (already-completed, from the operator's
   * perspective) submit flow.
   * What: two attempts, `ACTIVATE_RETRY_DELAY_MS` apart. Returns `null` on
   * success (either attempt); a caller-facing warning string if BOTH fail.
   * `signal` aborts the wait AND the fetch identically to every other call
   * in [`submit`].
   * Test: `StartWorkingForm.test.ts`.
   */
  async function activateWithRetry(
    base: string,
    workstreamId: string,
    signal: AbortSignal,
  ): Promise<string | null> {
    let lastWarning: string | null = null;
    for (let attempt = 0; attempt < 2; attempt += 1) {
      if (attempt > 0) {
        await new Promise((resolve) => setTimeout(resolve, ACTIVATE_RETRY_DELAY_MS));
        if (signal.aborted) return null;
      }
      try {
        const actRes = await fetch(`${base}/workstreams/${workstreamId}/activate`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ force: true }),
          signal,
        });
        if (signal.aborted) return null;
        if (actRes.ok) return null;
        lastWarning = `could not activate the new workstream (HTTP ${actRes.status})`;
      } catch (e) {
        if (signal.aborted) return null;
        lastWarning = e instanceof Error ? e.message : String(e);
      }
    }
    return lastWarning;
  }

  /**
   * The chat-continuation submit path (issue #3446): try to resume
   * `lastSessionId` as a follow-up turn; when there is no session to
   * resume (or the one there was is verified TERMINAL/vanished), run a
   * fresh session under the SAME already-active workstream instead —
   * never a second `POST /workstreams`.
   *
   * Why: see the module doc's "Chat continuation" section for the full
   * investigation of what the daemon supports, and its HIGH-1 section
   * (code-critic PR #3460 review) for why a rejected reuse MUST be
   * disambiguated before any fallback: the daemon returns the same
   * invalid-argument rejection for "terminal" and "still running", and
   * blindly minting on the latter forks a second concurrent session while
   * orphaning the running one.
   * What: three stages —
   * 1. Reuse (only when `lastSessionId` is set): `POST /tasks{session_id}`.
   *    `202` -> done (a genuine follow-up turn in the same session).
   * 2. Disambiguation probe on rejection: `GET /sessions/{lastSessionId}`.
   *    `status === "running"` -> BLOCK with a visible "still running"
   *    message, no mint. Terminal status or `404` -> proceed to stage 3
   *    with a visible fresh-session notice. Probe unreachable/malformed ->
   *    refuse to mint blind, surface the error, no mint.
   * 3. Workstream-targeted run: `POST /tasks{workstream_id:
   *    continuationWorkstreamId}` — also entered DIRECTLY (skipping 1–2)
   *    when `lastSessionId` is null but a continuation workstream is set,
   *    i.e. after the active workstream changed under this form (HIGH 2 —
   *    ambient targeting of the workstream the operator switched to).
   * Returns `true` on success after updating `lastSessionId` to whatever
   * session the SUCCESSFUL call actually ran against, `false` on failure
   * (with `submitError` already set).
   * Test: `StartWorkingForm.test.ts`.
   */
  async function submitContinuation(base: string, signal: AbortSignal): Promise<boolean> {
    let fallbackNotice: string | null = null;

    if (lastSessionId) {
      const reuseBody = buildRunTaskBody(task, selectedProjectState.project, null, lastSessionId, selectedAgent);
      const reuseRes = await fetch(`${base}/tasks`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(reuseBody),
        signal,
      });
      if (signal.aborted) return false;

      if (reuseRes.status === 202) {
        const created: unknown = await reuseRes.json().catch(() => null);
        if (signal.aborted) return false;
        lastSessionId = extractTaskSessionId(created) ?? lastSessionId;
        return true;
      }

      // Reuse rejected — disambiguate BEFORE any fallback (code-critic PR
      // #3460 review, HIGH 1; see the doc above).
      const reuseError = await errorMessage(reuseRes);
      if (signal.aborted) return false;

      let probedStatus: string | null = null;
      let sessionVanished = false;
      try {
        const probe = await fetch(`${base}/sessions/${lastSessionId}`, { signal });
        if (signal.aborted) return false;
        if (probe.status === 404) {
          sessionVanished = true;
        } else if (probe.ok) {
          const detail = (await probe.json().catch(() => null)) as { status?: unknown } | null;
          if (typeof detail?.status === 'string') probedStatus = detail.status;
        }
      } catch {
        // Probe unreachable — handled below as "could not verify".
      }
      if (signal.aborted) return false;

      if (probedStatus === 'running') {
        submitError =
          'a task is still running in this conversation — wait for it to finish (or cancel it above) before sending a follow-up';
        return false;
      }
      if (!sessionVanished && probedStatus === null) {
        // Could not verify the session's real state — refusing to mint
        // blind: if it IS still running, a mint here would orphan it.
        submitError = `${reuseError} — could not verify the session's status, so no new session was started; retry in a moment`;
        return false;
      }
      if (!continuationWorkstreamId) {
        submitError = reuseError;
        return false;
      }
      // Verified terminal (or vanished): a fresh session under the same
      // workstream is honest — and the operator gets told (the critic's
      // "at minimum surface a visible warning when a fallback mint
      // occurs"; here it is the designed path, verified safe first).
      fallbackNotice = sessionVanished
        ? 'previous session no longer exists — started a fresh one in this workstream'
        : `previous session ended (${probedStatus}) — started a fresh one in this workstream`;
      // The old session is verified unusable — drop it now so a future
      // submit never re-attempts (and re-probes) it even if the fallback
      // response below carries no session id.
      lastSessionId = null;
    }

    if (!continuationWorkstreamId) {
      // Unreachable via [`submit`]'s dispatch guard, but kept as a real
      // error rather than a silent no-op if that invariant ever breaks.
      submitError = 'no active workstream to continue';
      return false;
    }

    const fallbackBody = buildRunTaskBody(
      task,
      selectedProjectState.project,
      continuationWorkstreamId,
      null,
      selectedAgent,
    );
    const fallbackRes = await fetch(`${base}/tasks`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(fallbackBody),
      signal,
    });
    if (signal.aborted) return false;

    if (fallbackRes.status !== 202) {
      submitError = await errorMessage(fallbackRes);
      return false;
    }
    const created: unknown = await fallbackRes.json().catch(() => null);
    if (signal.aborted) return false;
    const newId = extractTaskSessionId(created);
    if (newId) lastSessionId = newId;
    if (fallbackNotice) successMessage = `started (${fallbackNotice})`;
    return true;
  }

  /**
   * The create sequence: mint (or reuse) a workstream, run the first task
   * bound to it, and only THEN activate it. Byte-for-byte the same
   * orchestration `NewWorkstreamForm.svelte` (issue #3365/PR #3375) shipped
   * — issue #3384 changed only the surrounding UI framing, issue #3446
   * changes only what happens on submits AFTER this one succeeds (see
   * [`submitContinuation`]).
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
  async function submitFirstMessage(base: string, signal: AbortSignal): Promise<boolean> {
    let workstreamId = pendingWorkstreamId;
    let workstreamName = pendingWorkstreamName;
    if (!workstreamId) {
      workstreamName = inferWorkstreamName(selectedProjectState.project);
      const wsRes = await fetch(`${base}/workstreams`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(buildCreateWorkstreamBody(workstreamName)),
        signal,
      });
      if (signal.aborted) return false;
      if (wsRes.status !== 201) {
        submitError = await errorMessage(wsRes);
        return false;
      }
      const wsCreated: unknown = await wsRes.json().catch(() => null);
      if (signal.aborted) return false;
      const newId = extractWorkstreamId(wsCreated);
      if (!newId) {
        submitError = 'workstream created but the daemon did not return an id';
        return false;
      }
      workstreamId = newId;
      pendingWorkstreamId = newId;
      pendingWorkstreamName = workstreamName;
    }
    // Record the fallback marker (code-critic PR #3392 review, MEDIUM)
    // whether this attempt just minted the workstream or is reusing one
    // from a prior create-succeeded-but-run-failed attempt — either way
    // `WorkstreamActivity.svelte` needs it available BEFORE the task-run
    // below, in case activation ultimately fails again.
    setPendingWorkstream(workstreamId, workstreamName ?? inferWorkstreamName(selectedProjectState.project));

    const body = buildRunTaskBody(task, selectedProjectState.project, workstreamId, null, selectedAgent);
    const res = await fetch(`${base}/tasks`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
      signal,
    });
    if (signal.aborted) return false;

    if (res.status !== 202) {
      // `pendingWorkstreamId`/`pendingWorkstreamName` deliberately stay
      // set here — the workstream exists; a retry must reuse it, not
      // mint another (code-critic PR #3375 review, HIGH).
      const daemonMessage = await errorMessage(res);
      submitError = `${daemonMessage} — workstream "${workstreamName}" was created but not activated; it will be reused if you retry.`;
      return false;
    }

    // Task run succeeded — chat continuation now targets this workstream/
    // session for every future submit (issue #3446).
    const created: unknown = await res.json().catch(() => null);
    if (signal.aborted) return false;
    continuationWorkstreamId = workstreamId;
    lastSessionId = extractTaskSessionId(created);

    // NOW activate, with one retry (best-effort, deliberately last; see the
    // doc above for why this is not the second step).
    const activationWarning = await activateWithRetry(base, workstreamId, signal);
    if (signal.aborted) return false;
    // Activation succeeded (either attempt) — the daemon's real active
    // pointer will reflect this workstream on the next poll, so the
    // fallback marker is no longer needed. Left SET on failure — that is
    // exactly the case `pending-workstream.svelte.ts` exists to cover.
    if (!activationWarning) clearPendingWorkstream();

    successMessage = activationWarning ? `started (${activationWarning})` : 'started';
    pendingWorkstreamId = null;
    pendingWorkstreamName = null;
    return true;
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

      // Continuation applies whenever this conversation already targets a
      // session OR a workstream (the latter alone after the active
      // workstream changed under the form — HIGH 2's re-target adoption);
      // only a form with neither mints a brand-new workstream. Both paths
      // thread `selectedAgent` (issue #3449) through to `buildRunTaskBody`
      // — see `submitFirstMessage`/`submitContinuation`'s own call sites
      // and `lib/new-workstream.ts::buildRunTaskBody`'s doc for why
      // `agent_name` is sent on every call, not just the mint path.
      const ok =
        lastSessionId || continuationWorkstreamId
          ? await submitContinuation(base, controller.signal)
          : await submitFirstMessage(base, controller.signal);
      if (controller.signal.aborted) return;
      if (!ok) return;

      // The response body (`{session_id, status, mode, binding}`) carries no
      // information this form still displays beyond what was already
      // extracted above — issue #3384 drops the "workstream created —
      // session <id>" ceremony message in favor of a plain "started";
      // `WorkstreamActivity.svelte` is where live status now lives.
      if (!successMessage) successMessage = 'started';
      task = '';
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

  $effect(() => {
    // One-shot, mount-only fetch of the agent catalog (issue #3449) — not
    // polled, unlike the tab components' rosters: this form's selector only
    // needs to be populated once before the operator submits, and a
    // mid-session catalog edit does not need to reflow an in-progress form.
    // Best-effort: any failure (daemon unreachable, etc.) just leaves the
    // selector at its "daemon default" option — the prior, only behavior.
    const controller = new AbortController();
    void (async () => {
      try {
        const base = await apiBase();
        const roster = await fetchAgentRoster(base, controller.signal);
        if (!controller.signal.aborted) agentRoster = roster;
      } catch {
        // Non-fatal — see comment above.
      }
    })();
    return () => controller.abort();
  });
</script>

<div class="ibar shrink-0 border-t border-1.5 border-trusty-border bg-trusty-card p-3">
  <div class="flex flex-wrap items-center gap-2 text-xs text-trusty-text-secondary">
    <span class="font-mono uppercase tracking-wide text-trusty-text-muted">project:</span>
    <span class="font-mono">{bindingLabel(selectedProjectState.project)}</span>
    <button
      type="button"
      onclick={openPicker}
      class="rounded-sm border-1.5 border-trusty-border-strong bg-trusty-raised px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-trusty-text-secondary hover:border-trusty-primary hover:text-trusty-primary"
    >
      choose project
    </button>
    {#if selectedProjectState.project}
      <button
        type="button"
        class="font-mono text-[11px] uppercase tracking-wide text-trusty-text-muted underline hover:text-trusty-primary"
        onclick={clearSelection}
      >
        clear
      </button>
    {/if}

    <!-- Issue #3449's `GET /agents` roster selector, carried over into the
         docked-bar layout (issue #3446/#3460) as a compact inline control
         in the same status row as the project picker, rather than the
         prior card layout's own labeled block — `selectedAgent` is threaded
         through every `buildRunTaskBody` call site (mint, reuse, and
         workstream-targeted fallback), so a pick here survives the whole
         conversation, not just the first message. -->
    <span class="font-mono uppercase tracking-wide text-trusty-text-muted">agent:</span>
    <select
      id="start-working-agent"
      bind:value={selectedAgent}
      title="GET /agents (issue #3449) — omitting this defers to the daemon's own default (DOC-39 §5.4), same as before this selector existed."
      class="rounded-sm border-1.5 border-trusty-border-strong bg-trusty-raised px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-trusty-text-secondary"
    >
      <option value="">daemon default</option>
      {#each agentRoster.filter((a) => a.tier !== 'broken') as agent (agent.name)}
        <!-- `broken` entries (unparseable disk files, issue #3449 review)
             are excluded: dispatching one is guaranteed to fail. -->
        <option value={agent.name}>{agent.name} ({agent.tier})</option>
      {/each}
    </select>
  </div>

  <div class="mt-2 flex items-end gap-2">
    <textarea
      id="start-working-task"
      bind:value={task}
      onkeydown={handleTaskKeydown}
      rows="2"
      placeholder="Type a message… (Enter to send, Shift+Enter for a new line)"
      class="min-w-0 flex-1 resize-none rounded-sm border-1.5 border-trusty-border-strong bg-trusty-raised p-2 text-xs text-trusty-text"
    ></textarea>
    <button
      type="button"
      disabled={!canSubmitCreate(task, submitPhase)}
      onclick={submit}
      class="shrink-0 self-stretch rounded-sm border-1.5 border-trusty-primary-hover bg-trusty-primary px-4 font-mono text-xs font-semibold uppercase tracking-wide text-trusty-text-inverse hover:bg-trusty-primary-hover disabled:cursor-not-allowed disabled:border-trusty-border disabled:bg-trusty-raised disabled:text-trusty-text-muted"
    >
      {submitPhase === 'submitting' ? 'sending…' : 'send'}
    </button>
  </div>

  {#if submitError}
    <p class="mt-1.5 text-xs text-status-error">{submitError}</p>
  {/if}
  {#if successMessage}
    <p class="mt-1.5 text-xs text-status-ok">{successMessage}</p>
  {/if}
</div>

<ProjectPickerModal open={pickerOpen} onSelect={onProjectSelected} onClose={closePicker} />
