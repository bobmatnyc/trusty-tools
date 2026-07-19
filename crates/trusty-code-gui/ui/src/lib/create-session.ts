// Why: `CreateSessionForm.svelte` (DOC-39 §4.2.1 the 7a folder picker,
// §6.2 item 6 `project.list_dir`/`fs.list_dir`, plus the minimal
// create+prompt form this slice adds — the GUI was observe-and-cancel
// only before this) needs typed wire shapes for `GET /fs` (mirrors
// `crate::fs_browse::{DirListing, DirEntryInfo}`) and a handful of pure,
// testable helpers: gating whether the create button may be clicked,
// building the `POST /tasks` body, and describing an `RpcError`-mapped
// HTTP status from `GET /fs` in one line. Extracted from the component for
// the same reason `session-status.ts`/`context-budget.ts` were: pure
// logic independent of DOM/network concerns is unit-testable without
// mounting anything.
//
// **`POST /tasks`, not `POST /sessions` (issue #3177).** The form used to
// call `POST /sessions` (`session.create`, REST Slice 3), which only mints a
// session record — it never spawns an agent loop, so every GUI-created
// session sat inert until something else called `task.run` against it (which
// nothing in the GUI did). `POST /tasks` (`task.run`,
// `crate::serve::rest::tasks`, #2983 Slice 4) is the one-shot "mint-or-reuse
// a session AND start executing" entry point DOC-39 §7A/AC-21 calls for —
// typing a task and hitting submit must actually run it. This module's
// `RunTaskBody`/[`buildRunTaskBody`]/[`extractTaskSessionId`]/
// [`isTaskRunResponse`] all target that route instead; see each doc comment
// for the wire-shape rationale. `project` (PR #3189, per-call project
// binding) is forwarded the same way `POST /sessions` already forwarded it —
// a mismatch against an existing session's own binding is rejected `400` by
// the daemon, surfaced through the component's existing generic
// `error.message` handling (no special-casing needed, the message is already
// caller-actionable: "project `<path>` does not match session `<id>`'s
// existing binding").
//
// **Picker mechanism note (DOC-39 §2.1 C-4).** The spec is explicit that a
// Tauri-native `fs`/dialog plugin is barred as a functional path — it would
// place filesystem capability in the UI layer and make the web build
// missing a capability the Tauri build has, which C-3 forbids. `GET /fs`
// (`crate::serve::rest::fs`, backed by `fs.list_dir`) is the daemon-served
// alternative DOC-39 §5.8/§4.2.1 calls for, already shipped on `origin/main`
// — this module and its component are a pure `fetch()` client over it, no
// Tauri command involved, identical in both shells (§2.1 C-3).
//
// **Agent-selection gap (task item 2, explicitly noted rather than
// invented).** `GET /sessions/{id}/agents` (`session.get_agents`) requires
// an existing `session_id` — there is no pre-session roster route. This
// form therefore omits `agent_name` from the `POST /tasks` body entirely,
// letting the daemon apply its own default (`task::protocol::task_run`
// defaults an absent `agent_name` the same way `session.create` did).
// Adding a roster endpoint that does not exist would violate DOC-39 §2.1 C-2
// ("a UI need with no API is an unbuilt feature, not a UI problem") — so
// this is recorded as a gap, not worked around.
//
// What: [`DirListing`]/[`DirEntryInfo`] mirror the daemon's wire shape
// field-for-field. [`ProjectSelection`] is the form's own selected-project
// state (always built from an already-listed, already-`is_dir`-confirmed
// entry — see the component's docs on why that makes a separate
// "validate this path" round-trip unnecessary: the listing call itself IS
// the exists+is-dir check DOC-39's task brief asks for). [`buildRunTaskBody`]
// mirrors `crate::serve::rest::tasks::RunTaskBody` minus `agent_name`;
// [`canSubmitCreate`] is the single gate the submit button's `disabled`
// binds to (non-empty task, not already in flight); [`bindingLabel`] is the
// small "projectless / directory / git repo" status line DOC-39 §4.2's
// three-state model implies; [`describeFsError`] turns a `GET /fs` HTTP
// status into the one-line message `rpc_error_to_status`'s four-way mapping
// (404/400/403/else) already promises the client.
// Test: `create-session.test.ts`.

/** Mirrors `crate::fs_browse::DirEntryInfo` field-for-field. */
export interface DirEntryInfo {
  name: string;
  path: string;
  is_dir: boolean;
  is_git_repo: boolean;
}

/** Mirrors `crate::fs_browse::DirListing` field-for-field — the
 * `GET /fs?path=..` response body. */
export interface DirListing {
  path: string;
  display_path: string;
  parent: string | null;
  entries: DirEntryInfo[];
}

/**
 * The form's selected-project state: always constructed from one
 * `DirEntryInfo` the daemon already returned in a successful listing, so
 * `isGitRepo` is exactly DOC-39 §4.2.1's binding discriminant and `path` is
 * already known to exist and be a directory — no separate validation call
 * is needed at submit time.
 */
export interface ProjectSelection {
  path: string;
  displayPath: string;
  isGitRepo: boolean;
}

/** Request body for `POST /tasks`, mirroring
 * `crate::serve::rest::tasks::RunTaskBody` minus `agent_name` (see the
 * module doc's gap note — this form never sends `agent_name`). */
export interface RunTaskBody {
  task_description: string;
  project?: string;
}

/** Whether the create-session submit control may be enabled. */
export type SubmitPhase = 'idle' | 'submitting';

/**
 * Build the `POST /tasks` request body from form state.
 *
 * Why: the single place task text is trimmed and an optional project path
 * is attached — kept out of the component so it's testable without
 * mounting Svelte.
 * What: `task_description` is trimmed (never sent with leading/trailing
 * whitespace); `project` is included only when `project` is non-null (a
 * `null` selection means projectless, AC-2.1 — the field is omitted
 * entirely rather than sent as `null`, matching `RunTaskBody`'s
 * `#[serde(default)]` Option handling on the Rust side). `agent_name` is
 * never sent (see the module doc's gap note).
 * Test: `create-session.test.ts::buildRunTaskBody`.
 */
export function buildRunTaskBody(task: string, project: ProjectSelection | null): RunTaskBody {
  const body: RunTaskBody = { task_description: task.trim() };
  if (project) body.project = project.path;
  return body;
}

/**
 * Whether the create-session button may be clicked right now.
 *
 * Why: the one gate `CreateSessionForm.svelte`'s submit button binds
 * `disabled` to — a non-empty (post-trim) task, and not already mid-flight
 * (double-submit guard, since `POST /tasks` starts a brand-new run on every
 * call).
 * What: `task.trim().length > 0 && phase === 'idle'`.
 * Test: `create-session.test.ts::canSubmitCreate`.
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
 * confirmed before they hit submit.
 * What: `null` -> `"projectless — chat/planning only"`; otherwise
 * `"git repo — <displayPath>"` or `"directory — <displayPath>"`.
 * Test: `create-session.test.ts::bindingLabel`.
 */
export function bindingLabel(project: ProjectSelection | null): string {
  if (!project) return 'projectless — chat/planning only';
  return project.isGitRepo ? `git repo — ${project.displayPath}` : `directory — ${project.displayPath}`;
}

/**
 * One-line description of a `GET /fs` failure, keyed on HTTP status.
 *
 * Why: `crate::serve::rest::rpc_error_to_status` maps `fs.list_dir`'s four
 * caller-actionable causes onto four distinct statuses (404/400/403/else) —
 * the client should not re-derive that from a raw status code inline in the
 * component, and should never substring-match a message to tell them apart.
 * What: `404` -> `"path not found"`; `400` -> `"not a directory"`; `403` ->
 * `"permission denied"`; anything else -> `"error (HTTP <status>)"`.
 * Test: `create-session.test.ts::describeFsError`.
 */
export function describeFsError(status: number): string {
  switch (status) {
    case 404:
      return 'path not found';
    case 400:
      return 'not a directory';
    case 403:
      return 'permission denied';
    default:
      return `error (HTTP ${status})`;
  }
}

/**
 * Runtime shape guard for a `GET /fs` 200 response body.
 *
 * Why: PR #3103 review finding (HIGH) — a bare `as DirListing` assertion let
 * a 200 response missing `entries` (schema drift, an interposed proxy, a
 * future daemon) flow into `listing.entries.filter(...)` and throw, and since
 * `CreateSessionForm` is mounted unconditionally in `App.svelte` that
 * TypeError took down the whole shell. A 200 status is a promise from THIS
 * daemon version's handler, not from whatever actually answered the socket —
 * the shape must be verified before it becomes reactive state.
 * What: structural check of every field the UI dereferences: `path` and
 * `display_path` are strings, `parent` is a string or null, `entries` is an
 * array whose members each carry string `name`/`path` and boolean
 * `is_dir`/`is_git_repo`.
 * Test: `create-session.test.ts::isDirListing`.
 */
export function isDirListing(body: unknown): body is DirListing {
  if (typeof body !== 'object' || body === null) return false;
  const b = body as Record<string, unknown>;
  if (typeof b.path !== 'string' || typeof b.display_path !== 'string') return false;
  if (b.parent !== null && typeof b.parent !== 'string') return false;
  if (!Array.isArray(b.entries)) return false;
  return b.entries.every((e: unknown) => {
    if (typeof e !== 'object' || e === null) return false;
    const entry = e as Record<string, unknown>;
    return (
      typeof entry.name === 'string' &&
      typeof entry.path === 'string' &&
      typeof entry.is_dir === 'boolean' &&
      typeof entry.is_git_repo === 'boolean'
    );
  });
}

/**
 * Runtime shape guard for a `POST /tasks` 202 response body.
 *
 * Why: same root pattern as [`isDirListing`] (PR #3103 review findings) — a
 * `202` status is only a promise that `task.run` accepted the request, not
 * that whatever answered the socket returned the shape this handler
 * dereferences. `crate::serve::rest::tasks::run_task`'s success body is
 * `{session_id, status, mode, binding}` (mirroring `task::protocol::task_run`
 * — see that module's `json!({...})`); the form only ever reads `session_id`
 * for its success message, so that is the one field validated as a non-empty
 * string before use.
 * What: `body` is an object and `body.session_id` is a string (may be
 * empty — emptiness is [`extractTaskSessionId`]'s concern, not the shape
 * guard's).
 * Test: `create-session.test.ts::isTaskRunResponse`.
 */
export function isTaskRunResponse(body: unknown): body is { session_id: string } {
  if (typeof body !== 'object' || body === null) return false;
  return typeof (body as Record<string, unknown>).session_id === 'string';
}

/**
 * Extract the session id from a `POST /tasks` 202 body, if any.
 *
 * Why: PR #3103 review finding (MEDIUM, same root pattern as [`isDirListing`])
 * — dereferencing a response field directly and slicing it in the template
 * throws when the body is missing or malformed. The `202` status is
 * authoritative that `task.run` was accepted; the id is only cosmetic, so its
 * absence must degrade the success message, never crash the form.
 * What: returns `body.session_id` when [`isTaskRunResponse`] accepts the
 * shape AND the id is non-empty, else `null`.
 * Test: `create-session.test.ts::extractTaskSessionId`.
 */
export function extractTaskSessionId(body: unknown): string | null {
  if (!isTaskRunResponse(body)) return null;
  return body.session_id.length > 0 ? body.session_id : null;
}
