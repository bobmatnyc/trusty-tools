import { writable, derived } from 'svelte/store';
import { apiBase } from '../lib/api-config';
import { invoke, isDesktop } from '../lib/transport';
import {
  BASE_AGENT_ID,
  buildRoster,
  type CatalogAgent,
  type OverlayAgent,
  type RosterEntry,
} from '../lib/roster';
import type { ModelsCatalogResponse, PickerEntry } from '../lib/models';

/**
 * Why: The sidebar needs a single source of truth for the list of chat
 * targets (CTRL + project PMs). Each `Project` entry maps to a separate
 * conversation with its own message history. CTRL has no `path` because it
 * runs with tagent's own cwd.
 * What: Minimal record describing what the sidebar renders and what gets
 * forwarded to the backend as `project_path`.
 * Test: Assert the initial store contains exactly one entry with id='ctrl'
 * and path=null.
 */
export interface SessionSummary {
  name: string;
  adapter_type: string;
  status: string;
}

export interface Project {
  id: string;
  name: string;
  path: string | null;
  status: 'idle' | 'running' | 'error';
  /** GET /api/projects fields (#341) — undefined for the local CTRL entry. */
  git_origin?: string;
  last_active?: string;
  open_issues_count?: number;
  open_prs_count?: number;
  framework?: string;
  sessions?: SessionSummary[];
}

export interface Message {
  id: string;
  role: 'user' | 'assistant' | 'system' | 'pm' | 'recap';
  content: string;
  timestamp: number;
  /** Task id returned by the backend; used to route progress events. */
  taskId?: string;
  /**
   * Why: Recap messages (#371) carry structured table rows we need to render
   * in ChatView as a styled banner. Keeping the rows on the Message itself
   * lets us replay chat history without re-querying the recap store.
   * What: Optional table rows for messages with `role === 'recap'`. Each row
   * is `[step_label, result_text]`, mirroring the SSE event shape.
   */
  recapRows?: [string, string][];
}

export interface TaskHistoryEntry {
  id: string;
  task: string;
  status: string;
  score?: number;
  score_max?: number;
  cost_usd?: number;
  narrative?: string;
  timestamp?: string;
}

/**
 * Why: "CTRL" is always present so the user lands on a usable chat even
 * before adding any project. Additional entries are appended via
 * `addProject()`.
 * What: Store holds the ordered list; `activeProjectId` selects which one the
 * chat view and input area operate on.
 * Test: Call `addProject({...})`, assert `$projects.length === 2` and the
 * second entry matches the input.
 */
export const projects = writable<Project[]>([
  { id: 'ctrl', name: 'CTRL', path: null, status: 'idle' },
]);

export const activeProjectId = writable<string>('ctrl');

/**
 * Why (#3223 — Trusty Assistant agent roster, epic #3052; regression fix
 * flagged by code-critic on PR #3279): the roster's "active agent" is a
 * second, independent selection axis alongside `activeProjectId` — which
 * conversation thread/working directory vs. which persona voice answers.
 * `null` — NOT the base `assistant` id — is the default: `agent_for_chat`
 * in `handlers.rs::submit_task` routes ANY non-`None` `agent` (including
 * `"assistant"`) through `run_pm_task_with_persona`, which is a tools-OFF
 * chat path (`persona.rs` — personas have no delegation-tool wiring yet).
 * Defaulting this store to the base id would therefore silently strip
 * delegation/tool capability from EVERY default GUI message, before the
 * user ever touches the roster switcher. `null` instead falls through to
 * `run_pm_task_with_session`, the existing tools-armed default path — the
 * roster only takes over dispatch once the user actively picks an entry
 * (including explicitly picking "Assistant", which is an intentional
 * opt-in to the tools-off persona path, not the passive default).
 * What: Holds the currently selected roster entry's `id`
 * (`RosterEntry.id` — either the base `assistant`, a catalog agent name, or
 * a user overlay's slug), or `null` when nothing has been explicitly
 * selected yet. `InputArea.svelte` and `lib/transport.ts`'s browser
 * fallback both OMIT the `agent` field from their submission payload when
 * this is `null`, rather than forwarding it.
 * Test: `AgentSwitcher.svelte` sets this on row click; `InputArea.svelte`
 * reads it into the submission payload (only when non-null).
 */
export const activeAgentId = writable<string | null>(null);

/** `GET /api/agents` catalog (`.trusty-agents/agents/*.toml`), #407. */
export const catalogAgents = writable<CatalogAgent[]>([]);

/**
 * Why: There is no REST route for a user's `$HOME` personalization overlay
 * tier (desktop-only filesystem access), so this is populated via the Tauri
 * `list_personalization_overlays` command instead of a `fetch*` helper.
 * Stays empty (not an error state) in browser/web mode.
 */
export const overlayAgents = writable<OverlayAgent[]>([]);

/**
 * Why: Both the roster switcher (#3223) and the agent editor (#3224) need
 * the SAME merged, deduplicated view of "base + catalog + overlays" — a
 * derived store recomputes it automatically whenever either source store
 * updates, so callers never have to remember to re-merge by hand.
 * What: `buildRoster($catalogAgents, $overlayAgents)` — see `lib/roster.ts`
 * for the merge/dedup rules.
 */
export const agentRoster = derived(
  [catalogAgents, overlayAgents],
  ([$catalogAgents, $overlayAgents]) => buildRoster($catalogAgents, $overlayAgents),
);

/**
 * Why: `GET /api/agents` is a plain REST endpoint (unlike the overlay tier),
 * so it's fetched the same way `fetchProjects`/`fetchTmSessions` are —
 * works in both Tauri and browser mode.
 * What: Fetches the envelope and replaces `catalogAgents`. Swallows nothing:
 * callers that want error feedback should catch around this call (mirrors
 * `fetchProjects`).
 * Test: Mount the roster switcher, observe a network call to
 * `/api/agents` and the store populated.
 */
export async function fetchAgentCatalog(): Promise<void> {
  const base = apiBase();
  const headers: Record<string, string> = {};
  const token = getCurrentApiToken();
  if (token) headers.Authorization = `Bearer ${token}`;
  const r = await fetch(`${base}/api/agents`, { headers });
  if (!r.ok) {
    throw new Error(`GET /api/agents failed: ${r.status}`);
  }
  const data = (await r.json()) as { agents?: CatalogAgent[] };
  catalogAgents.set(data.agents ?? []);
}

/**
 * Why: The overlay tier is direct `$HOME` filesystem access — only
 * available in the Tauri desktop shell (see `isDesktop()` in
 * `lib/transport.ts`), so this is a no-op in browser/web mode rather than an
 * error. Called on the roster switcher's mount and again after any
 * create/edit/delete in the agent editor so the roster reflects the change
 * immediately.
 * What: Invokes `list_personalization_overlays` and replaces
 * `overlayAgents`. Failures are logged, not thrown — a broken overlay scan
 * shouldn't block the rest of the roster from rendering.
 * Test: Manual — in the Tauri app, create an overlay via the agent editor,
 * confirm the switcher's roster includes it without a page reload.
 */
export async function refreshOverlayAgents(): Promise<void> {
  if (!isDesktop()) return;
  try {
    const list = await invoke<OverlayAgent[]>('list_personalization_overlays');
    overlayAgents.set(list ?? []);
  } catch (e) {
    console.error('[app] refreshOverlayAgents failed:', e);
  }
}

export type { RosterEntry };
export { BASE_AGENT_ID };

/**
 * Why (#3245 — model/provider picker, epic #3052; depends on #3243's
 * `GET /api/models` catalog): mirrors `activeAgentId`'s "second, independent
 * selection axis" pattern — which model/provider answers, alongside which
 * project (`activeProjectId`) and which persona (`activeAgentId`). `null`
 * is the default so a session that never touches the picker sends neither
 * `model_id` nor `provider_id` in `POST /api/task`, preserving the
 * pre-#3245 default (the agent config's own model, normal env-credential
 * resolution) byte-for-byte.
 * What: Holds the `PickerEntry` (see `lib/models.ts`) the user last
 * selected in `ModelSwitcher.svelte`, or `null`. `InputArea.svelte` maps it
 * through `resolveOverride()` into the submission payload.
 * Test: `ModelSwitcher.svelte` sets this on row click; `InputArea.svelte`
 * reads it into the submission payload (only when non-null and selectable).
 */
export const activeModelEntry = writable<PickerEntry | null>(null);

/** `GET /api/models` catalog (#3243), populated on `ModelSwitcher` mount. */
export const modelCatalog = writable<ModelsCatalogResponse | null>(null);

/**
 * Why: Same shape as `fetchAgentCatalog` — a plain REST fetch populating a
 * store, callable from `ModelSwitcher`'s `onMount` and any manual refresh.
 * What: Fetches `GET /api/models` and replaces `modelCatalog`. Throws on a
 * non-2xx response or network failure; callers (mirroring `fetchAgentCatalog`
 * call sites) catch around it so an unreachable API doesn't crash the input
 * area — the picker just falls back to showing only "Default".
 * Test: Mount the model switcher, observe a network call to `/api/models`
 * and the store populated.
 */
export async function fetchModelCatalog(): Promise<void> {
  const base = apiBase();
  const headers: Record<string, string> = {};
  const token = getCurrentApiToken();
  if (token) headers.Authorization = `Bearer ${token}`;
  const r = await fetch(`${base}/api/models`, { headers });
  if (!r.ok) {
    throw new Error(`GET /api/models failed: ${r.status}`);
  }
  const data = (await r.json()) as ModelsCatalogResponse;
  modelCatalog.set(data);
}

export type { ModelsCatalogResponse, PickerEntry };

/**
 * Why: When `tagent --api --api-token …` runs, every `/api/*` call
 * (except `/api/health` and `/api/config`) needs `Authorization: Bearer
 * <token>`. We persist the token in `localStorage` so the user only pastes
 * it once per browser, and expose it via this store so `transport.ts` and
 * any future API caller can read it reactively. (#181)
 * What: A writable store seeded from `localStorage['tagent.apiToken']`.
 * `setApiToken` updates both the store and storage in one call.
 * Test: `setApiToken('abc')`, reload page, assert `apiToken` initial value
 * is `'abc'`.
 */
const TOKEN_KEY = 'tagent.apiToken';
const initialToken =
  typeof localStorage !== 'undefined' ? localStorage.getItem(TOKEN_KEY) ?? '' : '';
export const apiToken = writable<string>(initialToken);

export function setApiToken(token: string): void {
  apiToken.set(token);
  if (typeof localStorage !== 'undefined') {
    if (token) localStorage.setItem(TOKEN_KEY, token);
    else localStorage.removeItem(TOKEN_KEY);
  }
}

/**
 * Why: `transport.ts` doesn't import Svelte stores (it's framework-agnostic
 * code), so we expose the current token via a synchronous getter that mirrors
 * the latest store value. We update this on every store write.
 */
let _currentToken = initialToken;
apiToken.subscribe((t) => {
  _currentToken = t;
});
export function getCurrentApiToken(): string {
  return _currentToken;
}

/** Set by `App.svelte` after probing `/api/config` on load. (#181) */
export const apiAuthRequired = writable<boolean>(false);

/** Per-project message log. Keyed by project id. */
export const messages = writable<Map<string, Message[]>>(new Map());

/** True while a task is in flight for the active project. Disables input. */
export const isRunning = writable<boolean>(false);

/**
 * Why: InputArea needs to know which task id is currently in flight so the
 * Stop button (#3063) can call `cancelTask(id)`, and Sidebar's "+ NEW TASK"
 * button (#3222) can warn before clearing context out from under a running
 * task. A plain writable (not per-project) matches the existing single
 * global `isRunning` flag — the GUI only ever drives one task at a time.
 * What: Set by InputArea when a submission starts (first to the client-side
 * placeholder id, then swapped to the backend's real id once the first
 * `task-progress` event reconciles it); cleared back to `null` once that
 * submission reaches a terminal state (complete/error/cancelled).
 * Test: Submit a message, assert `get(activeTaskId)` is non-null while
 * running and `null` again after `task-complete`/`task-error` fires.
 */
export const activeTaskId = writable<string | null>(null);

/** Recent tasks from the server's `/api/tasks` endpoint. */
export const taskHistory = writable<TaskHistoryEntry[]>([]);

export const activeProject = derived(
  [projects, activeProjectId],
  ([$projects, $activeProjectId]) =>
    $projects.find((p) => p.id === $activeProjectId) ?? $projects[0],
);

export const activeMessages = derived(
  [messages, activeProjectId],
  ([$messages, $activeProjectId]) => $messages.get($activeProjectId) ?? [],
);

/**
 * Why: Messages are appended from user input, assistant replies, and progress
 * events; funneling through a single helper keeps reactivity guaranteed
 * (replace the outer Map so Svelte detects the change).
 * What: Pushes `message` onto the list for `projectId`, creating the entry if
 * needed.
 * Test: Call `addMessage('ctrl', {role:'user',content:'hi', ...})`, assert
 * `get(messages).get('ctrl')?.length === 1`.
 */
export function addMessage(projectId: string, message: Message): void {
  messages.update((map) => {
    const next = new Map(map);
    const list = [...(next.get(projectId) ?? []), message];
    next.set(projectId, list);
    return next;
  });
}

/**
 * Why: Progress events arrive while a task is running; we find the assistant
 * placeholder for that task id and append/replace its content to grow the
 * bubble in place rather than spamming new messages.
 * What: Updates the first message matching `taskId` inside `projectId` with
 * the new content.
 * Test: Add a placeholder with `taskId='t1'`, call `updateMessageByTask` with
 * the same id and new content, assert the message content is replaced.
 */
export function updateMessageByTask(projectId: string, taskId: string, content: string): void {
  messages.update((map) => {
    const list = map.get(projectId);
    if (!list) return map;
    const next = new Map(map);
    next.set(
      projectId,
      list.map((m) => (m.taskId === taskId ? { ...m, content } : m)),
    );
    return next;
  });
}

/**
 * Why: When a message is first added to the store it carries a client-side
 * placeholder task id (e.g. `pending-<ts>`) because the backend hasn't yet
 * returned the real id. As soon as the first `task-progress` event arrives
 * with the real id, we need to swap it in so subsequent progress events —
 * which `updateMessageByTask` matches on `taskId` — actually find the bubble.
 * What: Replaces the `taskId` of the message currently tagged with
 * `oldTaskId` (within `projectId`) so future progress events route correctly.
 * Test: Add a message with `taskId='pending-1'`, call
 * `replaceMessageTaskId('ctrl', 'pending-1', 'real-abc')`, then
 * `updateMessageByTask('ctrl', 'real-abc', 'hi')`, assert content updates.
 */
export function replaceMessageTaskId(
  projectId: string,
  oldTaskId: string,
  newTaskId: string,
): void {
  messages.update((map) => {
    const list = map.get(projectId);
    if (!list) return map;
    const next = new Map(map);
    next.set(
      projectId,
      list.map((m) => (m.taskId === oldTaskId ? { ...m, taskId: newTaskId } : m)),
    );
    return next;
  });
}

export function addProject(project: Project): void {
  projects.update((list) => [...list, project]);
}

export function setProjectStatus(id: string, status: Project['status']): void {
  projects.update((list) => list.map((p) => (p.id === id ? { ...p, status } : p)));
}

/**
 * Why: ProjectsView needs the server's enriched project list (origin, issue
 * counts, sessions) without conflating it with the sidebar's `projects` store
 * (which holds only chat targets). A separate writable store keeps the two
 * concerns independent and lets refresh be triggered from any component. (#341)
 * What: Holds the array returned by `GET /api/projects`. `fetchProjects` calls
 * the endpoint and updates the store; pass `all=true` to include inactive
 * projects.
 * Test: Mount ProjectsView, click refresh, observe network call to
 * `/api/projects` and store update.
 */
export const projectsList = writable<Project[]>([]);

export async function fetchProjects(all = false): Promise<void> {
  const base = apiBase();
  const url = `${base}/api/projects${all ? '?all=true' : ''}`;
  const headers: Record<string, string> = {};
  const token = getCurrentApiToken();
  if (token) headers.Authorization = `Bearer ${token}`;
  const r = await fetch(url, { headers });
  if (!r.ok) {
    throw new Error(`GET /api/projects failed: ${r.status}`);
  }
  const data = (await r.json()) as Project[];
  projectsList.set(data);
}

/**
 * Why: The WebUI needs to call the new `/api/tm/*` endpoints (create, kill,
 * pause, resume, send, capture, list) for live session management (#450).
 * Centralising the fetch wrapper here keeps auth-token handling and base-URL
 * resolution in one place rather than duplicating per-component.
 * What: Fires the request with the auth header (when set), parses JSON, and
 * throws a descriptive error on non-2xx so callers can surface a toast/banner.
 * Returns the JSON body. Caller is responsible for typing.
 * Test: Mock fetch, call with `path='/api/tm/sessions'`, assert URL + headers.
 */
export async function tmApi<T = unknown>(
  path: string,
  init: RequestInit = {},
): Promise<T> {
  const base = apiBase();
  const headers: Record<string, string> = {
    ...((init.headers as Record<string, string>) ?? {}),
  };
  if (init.body && !headers['Content-Type']) {
    headers['Content-Type'] = 'application/json';
  }
  const token = getCurrentApiToken();
  if (token) headers.Authorization = `Bearer ${token}`;
  const r = await fetch(`${base}${path}`, { ...init, headers });
  const text = await r.text();
  let body: unknown;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    body = text;
  }
  if (!r.ok) {
    const errMsg =
      (body && typeof body === 'object' && 'error' in body
        ? String((body as { error: unknown }).error)
        : null) ?? `${init.method ?? 'GET'} ${path} failed: ${r.status}`;
    throw new Error(errMsg);
  }
  return body as T;
}

export interface TmSession {
  name: string;
  project: string;
  adapter_type: string;
  status: string;
  last_active: string;
}

/**
 * Why: ProjectsView polls live tmux state every 10s and merges it into the
 * project cards so session statuses (Running/Idle/Paused) stay fresh without
 * a manual refresh. Holding this in a dedicated store (not `projectsList`)
 * lets the project-tree fetch run on a different cadence and degrades
 * gracefully when tmux isn't available (store stays empty).
 * What: `tmSessions` holds the latest `/api/tm/sessions` payload;
 * `fetchTmSessions` populates it. Errors surface to the caller — the poller
 * in ProjectsView swallows them quietly so a transient 503 doesn't blank
 * the UI.
 * Test: Call `fetchTmSessions`, observe `tmSessions` updated; assert empty
 * list when API returns 503.
 */
export const tmSessions = writable<TmSession[]>([]);

export async function fetchTmSessions(): Promise<void> {
  const data = await tmApi<{ sessions: TmSession[] }>('/api/tm/sessions');
  tmSessions.set(data.sessions ?? []);
}
