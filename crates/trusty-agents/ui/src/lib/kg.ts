// Per-agent Knowledge Graph browser API client (#4290).
//
// Why: The Knowledge Graph browser needs a typed surface over the read-only
// proxy routes `crates/trusty-agents/src/api/server/agent_kg.rs` exposes
// (`GET /api/agents/:name/kg/subjects`, `/kg/all`, `/kg`, `/kg/count`). The
// SPA must never call trusty-memory directly — every cross-daemon read in
// this codebase goes through the trusty-agents sidecar (see
// `fetchAgentStores`, `fetchAgentSkills`), and the KG routes are no
// exception. Kept in its own module (rather than folded into
// `agentConfig.ts`) because these routes are not part of the agent-config
// five-section surface — they back a standalone slide-over opened from
// `ChatHeader`, not a config pane tab.
// What: `KgTriple`/`KgSubjectCount`/`KgActiveCount` mirror the upstream
// trusty-memory KG shapes verbatim (the proxy passes them through
// unreshaped); `KgEnvelope<T>` is the `{palace, connected, data}` wrapper
// every route returns. `fetchKgSubjects`/`fetchKgAll`/`fetchKgSubject`/
// `fetchKgCount` follow `fetchAgentStores`'s idiom exactly (agentConfig.ts:
// 200-213): `null` on a 404 (unknown agent — a normal outcome for a stale
// selection), throw on any other network/HTTP error. `connected: false` in a
// successfully-parsed envelope is NOT an error — it's a first-class state
// carrying a machine-readable `reason` the caller renders directly, per the
// route's never-fail posture.
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

/** One resolved KG triple, verbatim from trusty-memory (`kg_routes.rs`). */
export interface KgTriple {
  subject: string;
  predicate: string;
  object: string;
  confidence?: number;
  valid_from?: string;
  provenance?: string;
}

/** One subject + its triple count, from `/kg/subjects`. */
export interface KgSubjectCount {
  subject: string;
  count: number;
}

/** `/kg/count`'s `data` object. */
export interface KgActiveCount {
  active: number;
}

/**
 * The `{palace, connected, data}` envelope every KG proxy route returns
 * (`agent_kg.rs`'s module doc). `connected: false` is a first-class,
 * non-error state carrying a human-readable `reason` — render it directly,
 * never as a generic failure or an empty list that looks like "no data".
 * `config_error` is present only when the agent's own `agent.toml` failed to
 * parse (in which case `connected` is also `false`).
 */
export interface KgEnvelope<T> {
  palace: string | null;
  connected: boolean;
  reason?: string;
  data: T;
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
 * `GET /api/agents/:name/kg?subject=<s>`. `subject` is REQUIRED by the
 * upstream route (a `400` otherwise) — callers must never pass an empty
 * string.
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
 * `GET /api/agents/:name/kg/count`. `data` is `{"active": N}`.
 * Test: `fetchKgCount_parses_active_count` (`kg.test.ts`).
 */
export async function fetchKgCount(name: string): Promise<KgEnvelope<KgActiveCount> | null> {
  return getKgEnvelope(`/api/agents/${encodeURIComponent(name)}/kg/count`);
}
