// Per-agent Knowledge Graph browser API client (#4290, #7430).
//
// Why: The Knowledge Graph browser needs a typed surface over the read-only
// routes `crates/trusty-agents/src/api/server/agent_kg.rs` exposes
// (`GET /api/agents/:name/kg/subjects`, `/kg/all`, `/kg`, `/kg/count`). The
// graph those routes serve is the assistant's OKG tree — triples AND
// definitions. It is NOT a memory palace: before #7430 the routes proxied
// trusty-memory's palace-scoped `kg_*` surface, so this pane showed the memory
// knowledge graph, which epic #7425 item (f) rules out. Kept in its own module
// (rather than folded into `agentConfig.ts`) because these routes are not part
// of the agent-config five-section surface — they back a standalone slide-over
// opened from `ChatHeader`, not a config pane tab.
// What: `KgTriple`/`KgDefinition`/`KgSubjectCount`/`KgActiveCount` mirror the
// route's shapes verbatim; `KgEnvelope<T>` is the `{tree, source, connected,
// data, definitions}` wrapper every route returns. `fetchKgSubjects`/
// `fetchKgAll`/`fetchKgSubject`/`fetchKgCount` follow `fetchAgentStores`'s
// idiom exactly (agentConfig.ts: 200-213): `null` on a 404 (unknown agent — a
// normal outcome for a stale selection), throw on any other network/HTTP
// error. `connected: false` in a successfully-parsed envelope is NOT an error —
// it's a first-class state carrying a machine-readable `reason` the caller
// renders directly, per the route's never-fail posture.
// Test: `kg.test.ts` covers this module's own fetch/parse contract.
// `KnowledgeGraphBrowser.test.ts` covers the caller-side regression — a
// `connected: false` envelope from any of these functions must not render as
// empty data (#4290 code-review finding).

import { apiBase } from './api-config';
import { getCurrentApiToken } from '../stores/app';

function authHeaders(): Record<string, string> {
  const token = getCurrentApiToken();
  return token ? { Authorization: `Bearer ${token}` } : {};
}

/** One relationship edge read out of the OKG tree (`stores/okg_graph.rs`). */
export interface KgTriple {
  subject: string;
  predicate: string;
  object: string;
  /** Tree-relative path of the entity file the edge came from. */
  provenance?: string;
}

/**
 * One entity definition — what a subject IS, as distinct from what it links to.
 * The owner's closure condition for #7430 is that the exposed graph carries
 * both halves, so every content route returns these beside the triples.
 */
export interface KgDefinition {
  subject: string;
  collection: string;
  slug: string;
  type?: string;
  summary?: string;
  path: string;
}

/** One subject + its triple count, from `/kg/subjects`. */
export interface KgSubjectCount {
  subject: string;
  count: number;
}

/** `/kg/count`'s `data` object — both halves of the graph, as totals. */
export interface KgActiveCount {
  active: number;
  definition_count?: number;
}

/**
 * The `{tree, source, connected, data, definitions}` envelope every KG route
 * returns (`agent_kg.rs`'s module doc). `connected: false` is a first-class,
 * non-error state carrying a human-readable `reason` — render it directly,
 * never as a generic failure or an empty list that looks like "no data".
 * `tree` is the OKG directory the graph was read from, and `source` is always
 * `"okg"` — the payload states what it is made of rather than leaving a reader
 * to infer it. `config_error` is present only when the agent's own `agent.toml`
 * failed to parse (in which case `connected` is also `false`).
 */
export interface KgEnvelope<T> {
  tree: string | null;
  source?: string;
  connected: boolean;
  reason?: string;
  data: T;
  definitions?: KgDefinition[];
  config_error?: string;
}

/**
 * Why: all four KG routes share the same null-on-404 / throw-otherwise
 * contract, so the shared GET is factored once rather than repeated four
 * times.
 * What: `null` on 404 (unknown agent). Throws on any other non-2xx or
 * network error. A 2xx response is always this envelope shape — the routes
 * never fail otherwise (see `agent_kg.rs`'s never-fail posture).
 */
async function getKgEnvelope<T>(path: string): Promise<KgEnvelope<T> | null> {
  const r = await fetch(`${apiBase()}${path}`, { headers: authHeaders() });
  if (r.status === 404) return null;
  if (!r.ok) throw new Error(`GET ${path} failed: ${r.status}`);
  return (await r.json()) as KgEnvelope<T>;
}

/**
 * `GET /api/agents/:name/kg/subjects?limit=N`.
 * Test: `fetchKgSubjects_returns_null_on_404`, `fetchKgSubjects_parses_envelope`
 * (`kg.test.ts`).
 */
export async function fetchKgSubjects(
  name: string,
  limit = 200,
): Promise<KgEnvelope<KgSubjectCount[]> | null> {
  const params = new URLSearchParams({ limit: String(limit) });
  return getKgEnvelope(`/api/agents/${encodeURIComponent(name)}/kg/subjects?${params}`);
}

/**
 * `GET /api/agents/:name/kg/all?limit=N&offset=N`.
 * Test: `fetchKgAll_forwards_limit_and_offset` (`kg.test.ts`).
 */
export async function fetchKgAll(
  name: string,
  limit = 50,
  offset = 0,
): Promise<KgEnvelope<KgTriple[]> | null> {
  const params = new URLSearchParams({ limit: String(limit), offset: String(offset) });
  return getKgEnvelope(`/api/agents/${encodeURIComponent(name)}/kg/all?${params}`);
}

/**
 * `GET /api/agents/:name/kg?subject=<s>`. `subject` is REQUIRED by the route
 * (a `400` otherwise) — callers must never pass an empty string.
 * Test: `fetchKgSubject_encodes_the_subject` (`kg.test.ts`).
 */
export async function fetchKgSubject(
  name: string,
  subject: string,
): Promise<KgEnvelope<KgTriple[]> | null> {
  const params = new URLSearchParams({ subject });
  return getKgEnvelope(`/api/agents/${encodeURIComponent(name)}/kg?${params}`);
}

/**
 * `GET /api/agents/:name/kg/count`. `data` is
 * `{"active": N, "definition_count": D}`.
 * Test: `fetchKgCount_parses_active_count` (`kg.test.ts`).
 */
export async function fetchKgCount(name: string): Promise<KgEnvelope<KgActiveCount> | null> {
  return getKgEnvelope(`/api/agents/${encodeURIComponent(name)}/kg/count`);
}
