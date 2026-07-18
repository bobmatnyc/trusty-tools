// Why: `SearchTab.svelte` (DOC-39 §4.7, 10d) needs the wire shape for
// `GET /sessions/{id}/search-audit` (issue #3072, PR #3107) — the tagged
// `SearchAuditRecord` enum `crate::events::SearchAuditRecord` defines
// (`#[serde(tag = "kind", rename_all = "snake_case")]`), plus the small
// per-kind formatting rules AC-7.2 needs (lane badge, hit count, latency).
// Extracted into its own module rather than inlined in the component, the
// same split already established by `session-status.ts`/`context-budget.ts`
// — pure/testable logic independent of DOM/polling concerns.
//
// CRITICAL house rule (this session's critic reviews on PR #3103): no bare
// `as` assertion on a network-derived body. A `200` status is a promise from
// THIS daemon version's handler, not from whatever actually answered the
// socket — the shape must be verified before it becomes reactive state.
// `isSearchAuditResponse` is the runtime shape guard, mirroring
// `create-session.ts::isDirListing`'s per-field structural check.
//
// What: `SearchAuditRecord` (discriminated union on `kind`), the
// `{search_audit: [...]}` response envelope, `isSearchAuditResponse` (the
// shape guard `SearchTab.svelte` calls before trusting a fetched body), and
// three pure per-record formatters (`auditLaneLabel`/`auditHitsLabel`/
// `auditLatencyLabel`) — `Recall` records carry no `lane`/`latency_ms` (they
// are not a lane search), so these normalize both variants onto AC-7.2's six
// shared columns without fabricating fields neither the wire type nor the
// daemon has. Reuses `transcript.ts::formatElapsed` for the "age" column
// rather than re-deriving duration formatting.
// Test: `search-audit.test.ts`.

/** One `SearchAuditRecord::Search` row — mirrors `crate::events::SearchAuditRecord::Search`. */
export interface SearchAuditSearchRecord {
  kind: 'search';
  agent: string;
  agent_id: string;
  lane: string;
  query: string;
  hit_count: number | null;
  latency_ms: number;
  at: string;
}

/** One `SearchAuditRecord::Recall` row — mirrors `crate::events::SearchAuditRecord::Recall`. */
export interface SearchAuditRecallRecord {
  kind: 'recall';
  agent: string;
  agent_id: string;
  query: string;
  result_count: number;
  injected_count: number;
  at: string;
}

/** Mirrors `crate::events::SearchAuditRecord` in full. */
export type SearchAuditRecord = SearchAuditSearchRecord | SearchAuditRecallRecord;

/** `GET /sessions/{id}/search-audit` 200 response envelope. */
export interface SearchAuditResponse {
  search_audit: SearchAuditRecord[];
}

/**
 * Structural check for one `SearchAuditRecord` — used only through
 * {@link isSearchAuditResponse}, never called directly by the component.
 *
 * Why: same root pattern as `create-session.ts::isDirListing` — a record
 * whose `kind` doesn't match either known variant, or whose fields don't
 * match that variant's expected types, must not silently pass through as a
 * usable row (`record.lane.toUpperCase()`-style dereferences later would
 * throw on `undefined`).
 * What: every field common to both variants (`agent`/`agent_id`/`query`/
 * `at`, all strings) is checked first; `kind === 'search'` additionally
 * requires `lane: string`, `hit_count: number | null`, `latency_ms: number`;
 * `kind === 'recall'` additionally requires `result_count`/`injected_count:
 * number`. Any other `kind` value fails.
 * Test: `search-audit.test.ts::isSearchAuditRecord-via-response`.
 */
function isValidRecord(value: unknown): value is SearchAuditRecord {
  if (typeof value !== 'object' || value === null) return false;
  const r = value as Record<string, unknown>;
  if (
    typeof r.agent !== 'string' ||
    typeof r.agent_id !== 'string' ||
    typeof r.query !== 'string' ||
    typeof r.at !== 'string'
  ) {
    return false;
  }
  if (r.kind === 'search') {
    return (
      typeof r.lane === 'string' &&
      (r.hit_count === null || typeof r.hit_count === 'number') &&
      typeof r.latency_ms === 'number'
    );
  }
  if (r.kind === 'recall') {
    return typeof r.result_count === 'number' && typeof r.injected_count === 'number';
  }
  return false;
}

/**
 * Runtime shape guard for a `GET /sessions/{id}/search-audit` 200 response
 * body.
 *
 * Why: PR #3103 review finding (HIGH, `isDirListing`) established the house
 * rule — a bare `as SearchAuditResponse` assertion lets a malformed body
 * (schema drift, an interposed proxy) flow straight into reactive state and
 * throw once the template dereferences a missing field. `SearchTab.svelte`
 * must degrade to an error state instead, never throw from its `$effect`.
 * What: `body.search_audit` must be an array whose every element passes
 * {@link isValidRecord}. Malformed input (not an object, missing/non-array
 * `search_audit`, or any element failing its per-kind check) returns
 * `false`.
 * Test: `search-audit.test.ts::isSearchAuditResponse`.
 */
export function isSearchAuditResponse(body: unknown): body is SearchAuditResponse {
  if (typeof body !== 'object' || body === null) return false;
  const b = body as Record<string, unknown>;
  return Array.isArray(b.search_audit) && b.search_audit.every(isValidRecord);
}

/**
 * AC-7.2's "lane" column value for one record.
 *
 * Why: only `Search` records carry a real lane (`lexical`/`semantic`/
 * `graph`); a `Recall` record is not a lane search at all, so fabricating
 * one would misrepresent the operation. `'recall'` is an honest label for
 * the row's actual kind, not a guessed lane.
 * Test: `search-audit.test.ts::auditLaneLabel`.
 */
export function auditLaneLabel(record: SearchAuditRecord): string {
  return record.kind === 'search' ? record.lane : 'recall';
}

/**
 * AC-7.2's "hits" column value for one record.
 *
 * Why: `Search.hit_count` is `Option<usize>` on the wire (a lane that
 * doesn't report a count, e.g. a graph traversal) — `null` renders as an
 * honest em dash, never a fabricated `0`. `Recall` has no `hit_count` at
 * all; its `result_count`/`injected_count` pair is the closest equivalent,
 * rendered together so "how many came back" and "how many were actually
 * injected into context" are both visible rather than picking one and
 * silently dropping the other.
 * Test: `search-audit.test.ts::auditHitsLabel`.
 */
export function auditHitsLabel(record: SearchAuditRecord): string {
  if (record.kind === 'search') {
    return record.hit_count === null ? '—' : String(record.hit_count);
  }
  return `${record.result_count} (${record.injected_count} injected)`;
}

/**
 * AC-7.2's "latency" column value for one record.
 *
 * Why: only `Search` carries `latency_ms`; a `Recall` record has no timing
 * field on the wire at all, so this renders an honest em dash rather than a
 * fabricated `0ms`.
 * Test: `search-audit.test.ts::auditLatencyLabel`.
 */
export function auditLatencyLabel(record: SearchAuditRecord): string {
  return record.kind === 'search' ? `${record.latency_ms}ms` : '—';
}
