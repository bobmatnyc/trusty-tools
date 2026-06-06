import { writable, derived } from 'svelte/store';
import { apiBase } from '../lib/api-config';

/**
 * Why: The sidebar needs a single source of truth for the list of chat
 * targets (CTRL + project PMs). Each `Project` entry maps to a separate
 * conversation with its own message history. CTRL has no `path` because it
 * runs with open-mpm's own cwd.
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
 * Why: When `open-mpm --api --api-token …` runs, every `/api/*` call
 * (except `/api/health` and `/api/config`) needs `Authorization: Bearer
 * <token>`. We persist the token in `localStorage` so the user only pastes
 * it once per browser, and expose it via this store so `transport.ts` and
 * any future API caller can read it reactively. (#181)
 * What: A writable store seeded from `localStorage['ompm.apiToken']`.
 * `setApiToken` updates both the store and storage in one call.
 * Test: `setApiToken('abc')`, reload page, assert `apiToken` initial value
 * is `'abc'`.
 */
const TOKEN_KEY = 'ompm.apiToken';
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
