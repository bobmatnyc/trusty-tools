// Why: `CreateSessionForm.svelte` (DOC-39 §4.2.1 the 7a folder picker,
// §6.2 item 6 `project.list_dir`/`fs.list_dir`, plus the minimal
// session-creation form this slice adds — the GUI was observe-and-cancel
// only before this) needs typed wire shapes for `GET /fs` (mirrors
// `crate::fs_browse::{DirListing, DirEntryInfo}`) and a handful of pure,
// testable helpers: gating whether the create button may be clicked,
// building the `POST /sessions` body, and describing an `RpcError`-mapped
// HTTP status from `GET /fs` in one line. Extracted from the component for
// the same reason `session-status.ts`/`context-budget.ts` were: pure
// logic independent of DOM/network concerns is unit-testable without
// mounting anything.
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
// form therefore omits `agent` from the `POST /sessions` body entirely,
// letting the daemon apply its own default (`session::protocol::create`
// treats an absent `agent` as `None`, matching `CreateBody`'s
// `#[serde(default)]`). Adding a roster endpoint that does not exist would
// violate DOC-39 §2.1 C-2 ("a UI need with no API is an unbuilt feature, not
// a UI problem") — so this is recorded as a gap, not worked around.
//
// What: [`DirListing`]/[`DirEntryInfo`] mirror the daemon's wire shape
// field-for-field. [`ProjectSelection`] is the form's own selected-project
// state (always built from an already-listed, already-`is_dir`-confirmed
// entry — see the component's docs on why that makes a separate
// "validate this path" round-trip unnecessary: the listing call itself IS
// the exists+is-dir check DOC-39's task brief asks for). [`buildCreateBody`]
// mirrors `crate::serve::rest::sessions_write::CreateBody` minus `agent`;
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

/** Request body for `POST /sessions`, mirroring
 * `crate::serve::rest::sessions_write::CreateBody` minus `agent` (see the
 * module doc's gap note — this form never sends `agent`). */
export interface CreateSessionBody {
  task: string;
  project?: string;
}

/** Whether the create-session submit control may be enabled. */
export type SubmitPhase = 'idle' | 'submitting';

/**
 * Build the `POST /sessions` request body from form state.
 *
 * Why: the single place task text is trimmed and an optional project path
 * is attached — kept out of the component so it's testable without
 * mounting Svelte.
 * What: `task` is trimmed (never sent with leading/trailing whitespace);
 * `project` is included only when `project` is non-null (a `null` selection
 * means projectless, AC-2.1 — the field is omitted entirely rather than
 * sent as `null`, matching `CreateBody`'s `#[serde(default)]` Option
 * handling on the Rust side).
 * Test: `create-session.test.ts::buildCreateBody`.
 */
export function buildCreateBody(task: string, project: ProjectSelection | null): CreateSessionBody {
  const body: CreateSessionBody = { task: task.trim() };
  if (project) body.project = project.path;
  return body;
}

/**
 * Whether the create-session button may be clicked right now.
 *
 * Why: the one gate `CreateSessionForm.svelte`'s submit button binds
 * `disabled` to — a non-empty (post-trim) task, and not already mid-flight
 * (double-submit guard, since `POST /sessions` mints a brand-new resource on
 * every call).
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
