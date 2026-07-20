// Why: issue #3449 — the Foundry GUI's Agents tab needs the daemon's full
// agent catalog (embedded ∪ disk, `crate::agents::protocol`) plus add/remove
// over the disk tier, and `StartWorkingForm.svelte` has carried a standing
// gap note since #3384/#3437 ("agent: daemon default — no pre-task agent
// roster endpoint exists yet") because no such endpoint existed. `GET
// /agents` (this ticket) closes that gap for BOTH callers — this module is
// the one wire-shape + transport layer both `AgentsTab.svelte` (full
// CRUD) and `StartWorkingForm.svelte`'s optional agent selector (list-only)
// share, mirroring `lib/workstreams.ts`'s split of network calls out of the
// component.
//
// What: `AgentCatalogEntry` mirrors `crate::agents::protocol::entry_json`'s
// wire shape field-for-field (`tier` is `"embedded" | "project" | "user"`).
// `fetchAgentRoster`/`createAgent`/`deleteAgent` are thin `fetch()` wrappers
// over the unprefixed `/agents*` REST routes
// (`crate::serve::rest::agent_catalog`) — no local caching, no derived
// state, matching every other `lib/*.ts` transport module in this crate.
// `createAgent`/`deleteAgent` surface the daemon's REST error envelope
// message (`{error: {message}}`) via `agentApiErrorMessage`, mirroring
// `StartWorkingForm.svelte`'s inline `errorMessage` helper — extracted here
// so both this module's own error paths and any future caller format
// daemon errors identically.
//
// Test: `agent-roster.test.ts`.

/** Mirrors `crate::agents::protocol`'s per-entry wire shape. `"broken"`
 * marks a disk file that failed to parse — dispatch of that name will fail
 * until it is fixed or deleted (`agents_list`'s docs; code-critic PR #3465
 * review, MEDIUM), so it is disk-tier for management purposes (removable)
 * but must never render as a healthy embedded/dispatchable entry. */
export interface AgentCatalogEntry {
  name: string;
  tier: 'embedded' | 'project' | 'user' | 'broken';
  description: string | null;
  model: string | null;
}

/** `GET /agents` response envelope. */
export interface AgentCatalogResponse {
  agents: AgentCatalogEntry[];
}

/**
 * Parse a REST error envelope's `error.message`, falling back to a bare HTTP
 * status when the body is missing/malformed — mirrors
 * `StartWorkingForm.svelte`'s identical inline helper.
 * Test: `agent-roster.test.ts::agentApiErrorMessage`.
 */
export async function agentApiErrorMessage(res: Response): Promise<string> {
  const body = (await res.json().catch(() => null)) as { error?: { message?: string } } | null;
  return body?.error?.message ?? `HTTP ${res.status}`;
}

/**
 * `GET /agents` — the full embedded ∪ disk agent catalog.
 * Test: `agent-roster.test.ts::fetchAgentRoster`.
 */
export async function fetchAgentRoster(
  base: string,
  signal?: AbortSignal,
): Promise<AgentCatalogEntry[]> {
  const res = await fetch(`${base}/agents`, { signal });
  if (!res.ok) throw new Error(await agentApiErrorMessage(res));
  const body = (await res.json()) as AgentCatalogResponse;
  return body.agents;
}

/**
 * `POST /agents` — create a disk-tier agent from a Markdown+frontmatter
 * body. Throws with the daemon's error message on any non-`201` (e.g. a
 * `403` embedded-name collision, a `409` already-exists conflict).
 * Test: `agent-roster.test.ts::createAgent`.
 */
export async function createAgent(
  base: string,
  name: string,
  content: string,
): Promise<AgentCatalogEntry> {
  const res = await fetch(`${base}/agents`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name, content }),
  });
  if (res.status !== 201) throw new Error(await agentApiErrorMessage(res));
  return (await res.json()) as AgentCatalogEntry;
}

/**
 * `DELETE /agents/{name}` — remove a disk-tier agent. Throws with the
 * daemon's error message on any non-`200` (e.g. a `403` for an embedded
 * name, a `404` for an unknown one).
 * Test: `agent-roster.test.ts::deleteAgent`.
 */
export async function deleteAgent(base: string, name: string): Promise<void> {
  const res = await fetch(`${base}/agents/${encodeURIComponent(name)}`, { method: 'DELETE' });
  if (!res.ok) throw new Error(await agentApiErrorMessage(res));
}
