// Why: DOC-39 §2/§8 reserve a workstream-switcher control slot in
// `AppHeader.svelte` (issue #3153 shell rebuild, PR #3301) that Phase 1
// left as a disabled placeholder because "Phase 1 has no workstream
// registry yet". DOC-48 (issue #3292) built that registry — domain model,
// persistence, activation-lock exclusivity, and the `workstream.*`
// RPC/REST surface (`crates/trusty-code/src/workstreams/`,
// `crate::serve::rest::workstreams`) — and explicitly deferred "GUI
// workstream switcher" to Phase C (DOC-48 §8), this issue (#3300). This
// module is the wire-shape + transport layer `WorkstreamSwitcher.svelte`
// is built on, split out the same way `session-status.ts`/
// `context-budget.ts` split transport/pure-logic from their components.
//
// What: `Workstream`/`WorkstreamListResponse` mirror
// `crate::workstreams::protocol::WorkstreamView` and `workstream.list`'s
// response shape field-for-field. `fetchWorkstreams`/`activateWorkstream`/
// `closeWorkstream`/`renameWorkstream` are thin `fetch()` wrappers over the
// unprefixed `/workstreams*` REST routes (`crate::serve::rest::workstreams`,
// DOC-48 §5.2) — no local caching, no derived state (DOC-39 §2.1 C-1). Every
// other `lib/*.ts` module in this crate (`session-status.ts`,
// `context-budget.ts`, `search-audit.ts`) leaves `fetch()` calls inline in
// the component and only extracts pure parsing/formatting logic; this
// module goes one step further and extracts the calls themselves, because
// the switcher is the first component with FOUR distinct network
// operations (list/activate/close/rename) plus an SSE subscription —
// worth making each one independently testable (`workstreams.test.ts`)
// rather than mockable only by mounting the full component.
//
// `ActiveConflictError` surfaces the `409` `workstream.activate` returns
// when a DIFFERENT workstream is already active (DOC-48 §6.1). Its wire
// body's `error.data.active_id` is NOT available here: `respond()`
// (`crate::serve::rest::mod::respond`) deliberately drops `data` from every
// REST error envelope, a pre-existing, documented choice this ticket does
// not change (widening it would affect every REST error path, not just
// this one route) — so the conflict UI can say "someone else activated a
// workstream", never name which one by id/name. `activateWorkstream` does
// NOT auto-retry with `force: true`; DOC-48 §6.1 calls activation "an
// EXPLICIT switch, never silent" and this issue's scope says "surface it,
// offer refresh" — never a silent force override.
//
// Test: `workstreams.test.ts`.

/** Mirrors `crate::workstreams::model::WorkstreamState` (`#[serde(rename_all
 * = "snake_case")]`). */
export type WorkstreamState = 'active' | 'idle' | 'closed';

/** Mirrors `crate::workstreams::protocol::WorkstreamView` field-for-field —
 * the shape `workstream.get`/`workstream.list`/`workstream.rename` all
 * return for one record. */
export interface Workstream {
  id: string;
  name: string;
  state: WorkstreamState;
  session_ids: string[];
  created_at: string;
  updated_at: string;
  metadata: Record<string, unknown>;
}

/** `GET /workstreams` response envelope (DOC-48 §5.1 `workstream.list`). */
export interface WorkstreamListResponse {
  active_workstream_id: string | null;
  workstreams: Workstream[];
}

/**
 * Thrown by [`activateWorkstream`] on a real HTTP `409` — DOC-48 §6.1's
 * activation-lock exclusivity model: a DIFFERENT workstream is already
 * active and this call did not pass `force: true`.
 * Test: `workstreams.test.ts::activateWorkstream throws ActiveConflictError on 409`.
 */
export class ActiveConflictError extends Error {
  constructor() {
    super('another workstream is already active');
    this.name = 'ActiveConflictError';
  }
}

/**
 * `GET /workstreams` (default `include_closed=false`, matching the
 * server-side default DOC-48 §5.1 documents — a closed workstream is not a
 * switch target).
 * Test: `workstreams.test.ts::fetchWorkstreams`.
 */
export async function fetchWorkstreams(
  base: string,
  signal?: AbortSignal,
): Promise<WorkstreamListResponse> {
  const res = await fetch(`${base}/workstreams`, { signal });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return (await res.json()) as WorkstreamListResponse;
}

/**
 * `POST /workstreams/{id}/activate` with `force: false` — always an
 * explicit, non-forcing switch (see module docs for why this never
 * auto-retries with `force: true`).
 * Test: `workstreams.test.ts::activateWorkstream`.
 */
export async function activateWorkstream(base: string, id: string): Promise<void> {
  const res = await fetch(`${base}/workstreams/${id}/activate`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ force: false }),
  });
  if (res.status === 409) throw new ActiveConflictError();
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
}

/**
 * `POST /workstreams/{id}/close` — irreversible (DOC-48 §4.4); the caller
 * is responsible for the two-step confirm UI, not this function.
 * Test: `workstreams.test.ts::closeWorkstream`.
 */
export async function closeWorkstream(base: string, id: string): Promise<void> {
  const res = await fetch(`${base}/workstreams/${id}/close`, { method: 'POST' });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
}

/**
 * `POST /workstreams/{id}/rename` -> the updated [`Workstream`] (issue
 * #3300, Phase C — the one verb DOC-48 §5.1 shipped alongside its first
 * caller rather than in Phase 1A).
 * Test: `workstreams.test.ts::renameWorkstream`.
 */
export async function renameWorkstream(
  base: string,
  id: string,
  name: string,
): Promise<Workstream> {
  const res = await fetch(`${base}/workstreams/${id}/rename`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name }),
  });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return (await res.json()) as Workstream;
}

/**
 * Display label for a workstream whose `name` is empty — `workstream.create`
 * defaults `name` to `""` when the caller omits it (DOC-48 §5.1), and an
 * empty button/row label is not a usable affordance.
 * What: the record's `name` if non-blank (after trimming whitespace),
 * otherwise `"(untitled workstream)"`.
 * Test: `workstreams.test.ts::workstreamLabel`.
 */
export function workstreamLabel(ws: Pick<Workstream, 'name'>): string {
  const trimmed = ws.name.trim();
  return trimmed.length > 0 ? trimmed : '(untitled workstream)';
}

/**
 * Status-dot Tailwind class for a workstream's [`WorkstreamState`] — reuses
 * the same `bg-status-*` token set `StatusBar.svelte`/`WorkstreamActivity.svelte`
 * already establish (`ok` = green settled, `warn` = amber in-progress,
 * `neutral` = closed/inactive).
 * Test: `workstreams.test.ts::workstreamStateDotClass`.
 */
export function workstreamStateDotClass(state: WorkstreamState): string {
  switch (state) {
    case 'active':
      return 'bg-status-ok';
    case 'idle':
      return 'bg-status-warn';
    case 'closed':
    default:
      return 'bg-status-neutral';
  }
}

/**
 * The wire envelope one `GET /workstreams/{id}/events` SSE frame carries
 * (mirrors `crate::workstreams::sse::WorkstreamEventEnvelope` field-for-
 * field). `payload` is left as a loosely-typed record — this module only
 * needs `payload.type` to decide whether to trigger a re-fetch (see
 * `isActivationSignal`), not the full `crate::events::Event` union.
 */
export interface WorkstreamEventEnvelope {
  session_id: string;
  event_type: string;
  payload: { type: string; [key: string]: unknown };
}

/**
 * Whether a decoded SSE envelope is one the switcher must react to by
 * re-fetching `GET /workstreams` — `workstream_activation_changed` (the
 * daemon-wide active pointer moved) or `workstream_state_inferred` (this
 * workstream's own computed state changed, e.g. it was closed by another
 * client).
 * Why: extracted as a pure predicate so the SSE `onmessage` handler in
 * `WorkstreamSwitcher.svelte` has nothing to unit-test around — the
 * component only calls `refresh()` when this returns `true`.
 * Test: `workstreams.test.ts::isActivationSignal`.
 */
export function isActivationSignal(envelope: WorkstreamEventEnvelope): boolean {
  return (
    envelope.payload.type === 'workstream_activation_changed' ||
    envelope.payload.type === 'workstream_state_inferred'
  );
}
