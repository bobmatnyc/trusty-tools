import { get, writable } from 'svelte/store';
import type { AppEvent } from '../lib/transport';

/**
 * Why: #3217 — `bridgeEventToWebBus` (App.svelte) flattens every structured
 * SSE event (`phase_started`/`phase_done`/`agent_*`) and every completed
 * task's `PmResponse` (`phases_completed`/`files_modified`/`metadata`) into
 * plain-text webBus messages before any component can see the shape. That
 * loses the data ChatView's phase checklist (#3218), RecapPanel's session
 * rail (#3219), and TaskHistory's phase stamps (#3221) all need. This store
 * is a parallel, additive sink: it preserves the structured shape so those
 * three components can render it directly, while the existing flattened
 * webBus path keeps working unchanged for the plain-text chat log.
 * What: A single `workflowState` writable tracking the *active* task's
 * phase checklist, per-agent activity, touched files, and token/cost
 * totals, fed by two independent sources:
 *   1. `handleWorkflowEvent(ev)` — live, low-fidelity updates from the raw
 *      `AppEvent` stream (browser/SSE mode only; phase/agent events carry
 *      name+status but not elapsed/cost/note).
 *   2. `applyTaskResult(resp)` — a full-fidelity backfill from a
 *      `PmResponse`-shaped payload (available in Tauri desktop mode via the
 *      `task-complete` event, which carries the entire server response).
 * Test: Manual — `crates/trusty-agents/ui` has no unit test harness wired
 * up yet (see README); verified by driving `pnpm dev` against a running
 * `tagent --api` server and observing the phase card / recap rail update
 * live. See PR description for the SSE-event -> store-field mapping table.
 */

export type WorkflowPhaseStatus = 'pending' | 'running' | 'done' | 'failed';

export interface WorkflowPhase {
  name: string;
  status: WorkflowPhaseStatus;
  /** Only known once the terminal `PmResponse` arrives (see `applyTaskResult`). */
  elapsedSecs?: number;
  costUsd?: number;
  note?: string;
}

/** Why #3219: three states is the full vocabulary the recap rail's dot needs. */
export type AgentActivityStatus = 'working' | 'idle' | 'failed';

export interface AgentActivity {
  agent: string;
  status: AgentActivityStatus;
  lastActivity?: string;
  updatedAt: number;
}

export interface WorkflowTokens {
  tokensIn: number;
  tokensOut: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  costUsd: number;
  model?: string;
}

export type WorkflowRunStatus = 'idle' | 'running' | 'done' | 'failed' | 'cancelled';

export interface WorkflowState {
  taskId: string | null;
  status: WorkflowRunStatus;
  phases: WorkflowPhase[];
  agents: AgentActivity[];
  filesTouched: string[];
  tokens: WorkflowTokens | null;
}

function initialState(): WorkflowState {
  return {
    taskId: null,
    status: 'idle',
    phases: [],
    agents: [],
    filesTouched: [],
    tokens: null,
  };
}

export const workflowState = writable<WorkflowState>(initialState());

/**
 * Why: A new task (or `/api/clear-context`, which reloads the page and thus
 * re-initializes this module anyway) must not show the previous task's
 * checklist/agents/files. Exported separately from `handleWorkflowEvent` so
 * it stays a one-line, obviously-correct reset call site.
 * What: Replaces `workflowState` with a fresh `initialState()`.
 * Test: Call `resetWorkflow()` after populating state, assert `phases`,
 * `agents`, `filesTouched` are empty and `tokens` is null.
 */
export function resetWorkflow(): void {
  workflowState.set(initialState());
}

function upsertPhase(name: string, status: WorkflowPhaseStatus): void {
  workflowState.update((s) => {
    const idx = s.phases.findIndex((p) => p.name === name);
    if (idx === -1) {
      return { ...s, phases: [...s.phases, { name, status }] };
    }
    const phases = s.phases.slice();
    phases[idx] = { ...phases[idx], status };
    return { ...s, phases };
  });
}

function upsertAgent(
  agent: string | undefined,
  status: AgentActivityStatus,
  lastActivity?: string,
): void {
  if (!agent) return;
  workflowState.update((s) => {
    const idx = s.agents.findIndex((a) => a.agent === agent);
    const entry: AgentActivity = { agent, status, lastActivity, updatedAt: Date.now() };
    if (idx === -1) {
      return { ...s, agents: [...s.agents, entry] };
    }
    const agents = s.agents.slice();
    agents[idx] = entry;
    return { ...s, agents };
  });
}

/**
 * SSE event -> store field mapping (browser/SSE mode; see PR description for
 * the full table):
 *   session_started  -> reset(), taskId, status='running'
 *   session_done      -> status='done'|'failed'|'cancelled' (from ev.status)
 *   session_cancelled -> status='cancelled'
 *   phase_started     -> upsert phases[name].status='running'
 *   phase_done        -> upsert phases[name].status='done'|'failed'
 *   agent_spawned     -> upsert agents[agent] = working, "starting…"
 *   agent_message     -> upsert agents[agent] = working, ev.text
 *   agent_done        -> upsert agents[agent] = idle, "done (<status>)"
 *   agent_failed      -> upsert agents[agent] = failed, ev.error
 *
 * Why: Called unconditionally from `bridgeEventToWebBus` (App.svelte) for
 * every incoming `AppEvent`, alongside (not instead of) the existing
 * flattened-text switch — this function never emits webBus messages itself.
 * What: Mutates `workflowState` in place per the mapping above. Unknown
 * event types are ignored (no default branch needed since files_modified /
 * metadata / phase elapsed+cost+note are not carried on the wire by these
 * events — see `applyTaskResult` for the full-fidelity backfill).
 *
 * #3257 code-critic MEDIUM: events are filtered to the session this store is
 * currently tracking before the switch runs. The broadcast SSE stream is
 * process-wide (`events::bus()`), not per-subscriber-filtered by session
 * unless the client opted into `?session_id=`, so a stale event from a
 * previous/foreign task racing in after a reset (or arriving out of order)
 * must not mutate the active task's checklist. `session_started` always
 * resets+adopts regardless of the current id (it's the definitive "new task"
 * signal). When `workflowState.taskId` is still `null` (bootstrap — no
 * `session_started` seen yet, e.g. a `phase_started` arriving first) the
 * event's session is adopted rather than discarded, so the store isn't stuck
 * waiting for an event type that may never come.
 * Test: Manual — see module doc comment.
 */
export function handleWorkflowEvent(ev: AppEvent): void {
  if (ev.type === 'session_started') {
    resetWorkflow();
    workflowState.update((s) => ({ ...s, taskId: ev.session_id ?? null, status: 'running' }));
    return;
  }

  if (ev.session_id) {
    const current = get(workflowState);
    if (current.taskId === null) {
      workflowState.update((s) => (s.taskId === null ? { ...s, taskId: ev.session_id ?? null } : s));
    } else if (ev.session_id !== current.taskId) {
      return; // stale/foreign session — ignore
    }
  }

  switch (ev.type) {
    case 'session_done': {
      const wireStatus = (ev.status as string) ?? 'success';
      const status: WorkflowRunStatus =
        wireStatus === 'cancelled' ? 'cancelled' : wireStatus === 'success' || wireStatus === 'partial' ? 'done' : 'failed';
      workflowState.update((s) => ({ ...s, status }));
      break;
    }
    case 'session_cancelled':
      workflowState.update((s) => ({ ...s, status: 'cancelled' }));
      break;
    case 'phase_started':
      if (ev.phase) upsertPhase(ev.phase, 'running');
      break;
    case 'phase_done':
      if (ev.phase) upsertPhase(ev.phase, ev.status === 'failed' ? 'failed' : 'done');
      break;
    case 'agent_spawned':
      upsertAgent(ev.agent, 'working', 'starting…');
      break;
    case 'agent_message':
      upsertAgent(ev.agent, 'working', ev.text);
      break;
    case 'agent_done':
      upsertAgent(ev.agent, ev.status === 'failed' ? 'failed' : 'idle', `done (${ev.status ?? 'ok'})`);
      break;
    case 'agent_failed':
      upsertAgent(ev.agent, 'failed', ev.error);
      break;
    default:
      break;
  }
}

/**
 * Why: The only place the full `phases_completed` (with `elapsed_secs` /
 * `cost_usd` / `note`), `files_modified`, and `metadata` (token/cost
 * totals) are available on the wire is the terminal `PmResponse` itself —
 * the `task-complete` event. In Tauri desktop mode that event's payload IS
 * the full `PmResponse` (see `ui/src-tauri/src/main.rs::send_message`); in
 * the browser fallback it's a narrower `{id,narrative,status}` shape, so
 * this function defensively no-ops on whichever fields are absent rather
 * than assuming a fixed shape.
 * What: Merges any present fields from a `PmResponse`-shaped payload into
 * `workflowState`. Phase list is replaced wholesale (it's the authoritative
 * final list, not an incremental patch); files/tokens likewise replace.
 * Test: Manual — see module doc comment.
 */
export interface PmResponseLike {
  id?: string;
  status?: string;
  files_modified?: string[];
  phases_completed?: Array<{
    name: string;
    status: string;
    elapsed_secs?: number;
    cost_usd?: number;
    note?: string | null;
  }>;
  metadata?: {
    total_tokens_in?: number;
    total_tokens_out?: number;
    cache_read_tokens?: number;
    cache_creation_tokens?: number;
    total_cost_usd?: number;
    model?: string | null;
  };
  /**
   * Why (#3257 code-critic HIGH / #3258): today's `PmResponse` (see
   * `crates/trusty-agents/src/api/types.rs`) carries no per-agent summary —
   * `AgentSpawned`/`AgentMessage`/`AgentDone`/`AgentFailed` only exist as
   * live SSE events (`events.rs`), which Tauri desktop mode never receives
   * (`App.svelte`'s SSE bridge is browser-only). So in desktop mode
   * `workflowState.agents` stays empty for the entire task, not just until
   * completion — `RecapPanel` renders an explicit "unavailable in desktop
   * mode" state rather than a misleading "No agents active." in that case
   * (see #3258, tracked as a follow-up to forward live agent events through
   * the Tauri bridge). This field does not exist on the wire yet; it's
   * declared here so that if/when the backend adds one, `applyTaskResult`
   * picks it up with zero changes to this store or its consumers.
   */
  agents_active?: Array<{ agent: string; status: string; last_activity?: string | null }>;
}

export function applyTaskResult(resp: unknown): void {
  if (!resp || typeof resp !== 'object') return;
  const r = resp as PmResponseLike;

  workflowState.update((s) => {
    const next: WorkflowState = { ...s };

    if (r.id) next.taskId = r.id;

    if (r.status) {
      next.status =
        r.status === 'cancelled'
          ? 'cancelled'
          : r.status === 'success' || r.status === 'partial'
            ? 'done'
            : r.status === 'running'
              ? 'running'
              : 'failed';
    }

    if (Array.isArray(r.phases_completed) && r.phases_completed.length > 0) {
      next.phases = r.phases_completed.map((p) => ({
        name: p.name,
        status:
          p.status === 'done'
            ? 'done'
            : p.status === 'failed'
              ? 'failed'
              : p.status === 'running'
                ? 'running'
                : 'pending',
        elapsedSecs: p.elapsed_secs,
        costUsd: p.cost_usd,
        note: p.note ?? undefined,
      }));
    }

    if (Array.isArray(r.files_modified)) {
      next.filesTouched = r.files_modified;
    }

    // See `PmResponseLike.agents_active` doc comment — no-op today (the
    // field doesn't exist on the wire), forward-compatible if it's added.
    if (Array.isArray(r.agents_active) && r.agents_active.length > 0) {
      next.agents = r.agents_active.map((a) => ({
        agent: a.agent,
        status: a.status === 'working' ? 'working' : a.status === 'failed' ? 'failed' : 'idle',
        lastActivity: a.last_activity ?? undefined,
        updatedAt: Date.now(),
      }));
    }

    if (r.metadata) {
      next.tokens = {
        tokensIn: r.metadata.total_tokens_in ?? 0,
        tokensOut: r.metadata.total_tokens_out ?? 0,
        cacheReadTokens: r.metadata.cache_read_tokens ?? 0,
        cacheCreationTokens: r.metadata.cache_creation_tokens ?? 0,
        costUsd: r.metadata.total_cost_usd ?? 0,
        model: r.metadata.model ?? undefined,
      };
    }

    return next;
  });
}
