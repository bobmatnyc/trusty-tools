// Why: `StartWorkingForm.svelte` (issue #3365 — Bob's product direction:
// "Let's call it a new workstream, and users pick a project or just start
// chatting… let's focus on 'workstreams' as the primary connection path, not
// sessions") replaces the prior session-first create+prompt flow
// (`CreateSessionForm.svelte`/`lib/create-session.ts`, DOC-39 §4.2.1's 7a
// folder picker) with a workstream-first one: pick a project (via
// `ProjectPickerModal.svelte`, `lib/project-roster.ts`) or go projectless,
// then submit mints a WORKSTREAM (`POST /workstreams`), runs the first task
// bound to it (`POST /tasks` with `workstream_id` — issue #3298/PR #3354's
// binding support, previously unused by any GUI caller), and only THEN
// activates it (`POST /workstreams/{id}/activate`). This module carries that
// flow's pure, testable logic — gating/body-construction/name-inference —
// the same split `session-status.ts`/`context-budget.ts` establish, renamed
// from `create-session.ts` to match the concept it now serves.
//
// **Why three sequential calls, not one.** DOC-48 has no single "create +
// run + activate" verb; each is its own REST route, matching the RPC
// surface's own Phase 1A/1B shape (`workstream.create`, `task.run`,
// `.activate`). `buildRunTaskBody`'s `workstream_id` is always passed
// EXPLICITLY (never left to DOC-48 §4.2's ambient active-workstream
// default) — the ambient path is documented as "RPC layer supports it, UI
// integration is Phase B+", and this ticket's job is exactly that UI
// integration, so it binds explicitly rather than relying on activation
// timing.
//
// **Activation is best-effort, LAST, and never re-mints on retry (see
// `StartWorkingForm.svelte`'s own docs for the full sequencing and the
// code-critic PR #3375 review that moved it here).** Activation is
// deliberately the FINAL step, after a successful task-run — not the second
// step — because `force: true` unconditionally deactivates whatever was
// previously active, and firing that before knowing the task-run will even
// succeed can strand an empty workstream as the new active one with no
// restore path. `task.run`'s `workstream_id` binds a session to a
// workstream by id alone, with no activation precondition
// (`task::protocol::task_run`'s docs), so deferring activation costs
// nothing and a failed activation still yields a fully valid,
// workstream-bound session. A task-run failure AFTER a successful create
// keeps the minted workstream id around for the next `submit()` call to
// reuse, rather than minting a second (orphaned) workstream per retry.
//
// **Project selection has no manual browse in this flow.** The prior form's
// `fs.list_dir`-backed folder browser (`DirListing`/`DirEntryInfo`/
// `isDirListing`/`describeFsError`) is dropped from this module — issue
// #3365 replaces it with `ProjectPickerModal`'s roster
// (`lib/project-roster.ts`, `GET /projects`) plus an explicit "start
// chatting without a project" affordance; `fs.list_dir`/`GET /fs` itself is
// untouched and still exists for any future manual-browse surface, just no
// longer called from this flow.
// What: [`ProjectSelection`] is the form's selected-project state (now
// always built from a [`ProjectCandidate`](./project-roster) the roster
// already returned, or `null` for projectless — no separate "does this path
// exist" round-trip, mirroring the prior form's identical reasoning for its
// own picker). [`inferWorkstreamName`] builds the default workstream name
// (DOC-48 §9 Q1 defers real name inference; this ticket's own brief asks
// for "project name + date" as the interim default — issue #3384 makes this
// MORE load-bearing, not less: the operator never types a name at all now).
// [`buildCreateWorkstreamBody`]/[`buildRunTaskBody`] mirror `POST
// /workstreams`'/`POST /tasks`' request bodies; [`canSubmitCreate`]/
// [`bindingLabel`] are unchanged carry-overs from the prior form (the gating
// rule and the "selected:" status line are identical concepts, just now
// populated by the modal instead of an inline browse);
// [`extractWorkstreamId`] extracts the id `submit()` needs to bind the task
// run and (later) activate, guarded by a runtime shape check for the same PR
// #3103 reasons `create-session.ts` originally documented. (Issue #3384:
// this module used to also export `isTaskRunResponse`/`extractTaskSessionId`
// for `POST /tasks`' response — removed as dead code once
// `StartWorkingForm.svelte` stopped reading a session id out of that body
// for its success message; the `POST /tasks` response shape itself is
// unchanged, only this module's need to parse it went away.)
// Test: `new-workstream.test.ts`.

/**
 * The form's selected-project state — either built from a
 * [`ProjectCandidate`](./project-roster) the roster already returned (`path`
 * is already known to exist and be a git repo — no separate validation call
 * is needed at submit time) or left `null` for projectless.
 */
export interface ProjectSelection {
  path: string;
  displayPath: string;
  isGitRepo: boolean;
}

/** Request body for `POST /workstreams`, mirroring
 * `crate::serve::rest::workstreams::CreateBody`. */
export interface CreateWorkstreamBody {
  name?: string;
}

/** Request body for `POST /tasks`, mirroring
 * `crate::serve::rest::tasks::RunTaskBody` minus `agent_name` (see the
 * module doc's carried-over gap note from `create-session.ts` — this form
 * still never sends `agent_name`; there is no pre-session roster route for
 * it, distinct from this ticket's own project roster). */
export interface RunTaskBody {
  task_description: string;
  project?: string;
  workstream_id?: string;
}

/** Whether the create-workstream submit control may be enabled. */
export type SubmitPhase = 'idle' | 'submitting';

/**
 * Infer a default workstream name from the selected project and the
 * current date.
 *
 * Why: DOC-48 §9 Q1 ("workstream name inference") defers real inference
 * (first-turn-prompt-derived naming) to a later phase; this ticket's own
 * brief asks for "a sensible default like project name + date" in the
 * meantime, so a workstream is never left literally unnamed (the switcher
 * already has to render SOMETHING for an empty name —
 * `workstreams.ts::workstreamLabel` falls back to
 * `"(untitled workstream)"` — inferring a real label here is strictly
 * better than relying on that fallback).
 * What: `"<project display path> — <YYYY-MM-DD>"` when a project is
 * selected, else `"new chat — <YYYY-MM-DD>"`. `now` defaults to the current
 * time but is an explicit param so tests can pin the date.
 * Test: `new-workstream.test.ts::inferWorkstreamName`.
 */
export function inferWorkstreamName(project: ProjectSelection | null, now: Date = new Date()): string {
  const date = now.toISOString().slice(0, 10);
  const label = project ? project.displayPath : 'new chat';
  return `${label} — ${date}`;
}

/**
 * Build the `POST /workstreams` request body.
 *
 * Why: kept out of the component so it's testable without mounting Svelte,
 * mirroring `buildRunTaskBody`'s own rationale.
 * What: `name` is trimmed and included only when non-empty — an
 * empty/whitespace-only name omits the field entirely, letting the daemon
 * apply its own empty-string default (`workstream.create`'s documented
 * behaviour) rather than sending a blank string explicitly.
 * Test: `new-workstream.test.ts::buildCreateWorkstreamBody`.
 */
export function buildCreateWorkstreamBody(name: string): CreateWorkstreamBody {
  const trimmed = name.trim();
  return trimmed.length > 0 ? { name: trimmed } : {};
}

/**
 * Build the `POST /tasks` request body from form state.
 *
 * Why: the single place task text is trimmed and the optional project/
 * workstream bindings are attached — kept out of the component so it's
 * testable without mounting Svelte (carried over from `create-session.ts`,
 * extended with `workstream_id` for issue #3365).
 * What: `task_description` is trimmed; `project` is included only when
 * `project` is non-null (a `null` selection means projectless — the field
 * is omitted entirely rather than sent as `null`); `workstream_id` is
 * included only when passed and non-empty. `agent_name` is never sent (see
 * the module doc's gap note).
 * Test: `new-workstream.test.ts::buildRunTaskBody`.
 */
export function buildRunTaskBody(
  task: string,
  project: ProjectSelection | null,
  workstreamId?: string | null,
): RunTaskBody {
  const body: RunTaskBody = { task_description: task.trim() };
  if (project) body.project = project.path;
  if (workstreamId) body.workstream_id = workstreamId;
  return body;
}

/**
 * Whether the create-workstream button may be clicked right now.
 *
 * Why: the one gate `StartWorkingForm.svelte`'s submit button binds
 * `disabled` to — a non-empty (post-trim) task, and not already mid-flight
 * (double-submit guard, since submitting starts a brand-new
 * create-workstream/activate/run-task sequence on every call).
 * What: `task.trim().length > 0 && phase === 'idle'`.
 * Test: `new-workstream.test.ts::canSubmitCreate`.
 */
export function canSubmitCreate(task: string, phase: SubmitPhase): boolean {
  return task.trim().length > 0 && phase === 'idle';
}

/**
 * One-line description of the current project selection, for the form's
 * "selected: …" status line.
 *
 * Why: DOC-39 §4.2's three-state binding model (`Projectless` /
 * `Bound · non-git` / `Bound · git repo`) is exactly what the operator needs
 * confirmed before they hit submit — carried over unchanged from
 * `create-session.ts`, since the underlying binding model this describes
 * did not change, only how a selection is made.
 * What: `null` -> `"projectless — chat/planning only"`; otherwise
 * `"git repo — <displayPath>"` or `"directory — <displayPath>"`.
 * Test: `new-workstream.test.ts::bindingLabel`.
 */
export function bindingLabel(project: ProjectSelection | null): string {
  if (!project) return 'projectless — chat/planning only';
  return project.isGitRepo ? `git repo — ${project.displayPath}` : `directory — ${project.displayPath}`;
}

/**
 * Runtime shape guard for a `POST /workstreams` 201 response body.
 *
 * Why: same root pattern as `project-roster.ts::isProjectRoster` (PR #3103
 * precedent) — a `201` status is only a promise that `workstream.create`
 * succeeded, not that whatever answered the socket returned the shape this
 * form dereferences (`crate::serve::rest::workstreams::create_workstream`'s
 * success body is `{"id": ..}`).
 * What: `body` is an object and `body.id` is a string (may be empty —
 * emptiness is [`extractWorkstreamId`]'s concern, not the shape guard's).
 * Test: `new-workstream.test.ts::isWorkstreamCreateResponse`.
 */
export function isWorkstreamCreateResponse(body: unknown): body is { id: string } {
  if (typeof body !== 'object' || body === null) return false;
  return typeof (body as Record<string, unknown>).id === 'string';
}

/**
 * Extract the workstream id from a `POST /workstreams` 201 body, if any.
 *
 * Why: the `201` status is authoritative that the workstream was created; a
 * missing/malformed id in the body must degrade the caller's next step to a
 * clear error, never throw.
 * What: returns `body.id` when [`isWorkstreamCreateResponse`] accepts the
 * shape AND the id is non-empty, else `null`.
 * Test: `new-workstream.test.ts::extractWorkstreamId`.
 */
export function extractWorkstreamId(body: unknown): string | null {
  if (!isWorkstreamCreateResponse(body)) return null;
  return body.id.length > 0 ? body.id : null;
}
