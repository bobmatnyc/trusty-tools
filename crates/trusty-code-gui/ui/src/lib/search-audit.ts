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
// `parseSearchAuditResponse` is the runtime shape guard, mirroring
// `project-roster.ts::isProjectRoster`'s per-field structural check.
//
// PARTIAL-VALIDITY note (issue #3111 MEDIUM, PR #3110 critic review): the
// first cut of this guard (`isSearchAuditResponse`, a `body is
// SearchAuditResponse` predicate) rejected the ENTIRE array when even one
// record failed its per-kind check — a single malformed row, or a future
// third `SearchAuditRecord` variant the client doesn't know about yet, took
// down every known-good row with it. `parseSearchAuditResponse` replaces it:
// only a TOP-LEVEL shape failure (body not an object, or `search_audit` not
// an array — cases where there is no array to even iterate) returns `null`;
// a record-level failure is filtered out of the returned subset and counted
// in `omittedCount` instead, so `SearchTab.svelte` can render every row it
// CAN trust plus an honest "N rows omitted" indicator, rather than an
// all-or-nothing "audit unavailable".
//
// What: `SearchAuditRecord` (discriminated union on `kind`), the
// `{search_audit: [...]}` response envelope, `parseSearchAuditResponse` (the
// shape guard/subset-filter `SearchTab.svelte` calls before trusting a
// fetched body), and three pure per-record formatters (`auditLaneLabel`/
// `auditHitsLabel`/`auditLatencyLabel`) — `Recall` records carry no
// `lane`/`latency_ms` (they are not a lane search), so these normalize both
// variants onto AC-7.2's six shared columns without fabricating fields
// neither the wire type nor the daemon has. Reuses
// `transcript.ts::formatElapsed` for the "age" column rather than
// re-deriving duration formatting.
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
 * {@link parseSearchAuditResponse}, never called directly by the component.
 *
 * Why: same root pattern as `project-roster.ts::isProjectRoster` — a record
 * whose `kind` doesn't match either known variant, or whose fields don't
 * match that variant's expected types, must not silently pass through as a
 * usable row (`record.lane.toUpperCase()`-style dereferences later would
 * throw on `undefined`).
 * What: every field common to both variants (`agent`/`agent_id`/`query`/
 * `at`, all strings) is checked first; `kind === 'search'` additionally
 * requires `lane: string`, `hit_count: number | null`, `latency_ms: number`;
 * `kind === 'recall'` additionally requires `result_count`/`injected_count:
 * number`. Any other `kind` value fails — including a hypothetical future
 * third variant this client build doesn't know about yet, which is exactly
 * the case {@link parseSearchAuditResponse} tolerates as "omit this one
 * row", not "reject the whole response".
 * Test: `search-audit.test.ts` (exercised indirectly through
 * `parseSearchAuditResponse`).
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

/** {@link parseSearchAuditResponse}'s result for a structurally-valid
 * top-level body: the trustworthy subset of records, plus how many were
 * dropped for failing {@link isValidRecord}. `omittedCount === 0` is the
 * common case (every record valid); a positive count means `SearchTab.svelte`
 * should render `records` plus a visible "N rows omitted" indicator, never
 * silently drop them with no trace. */
export interface ParsedSearchAudit {
  records: SearchAuditRecord[];
  omittedCount: number;
}

/**
 * Runtime shape guard AND subset-filter for a `GET /sessions/{id}/search-audit`
 * 200 response body.
 *
 * Why: PR #3103 review finding (HIGH, `isDirListing`) established the house
 * rule — a bare `as SearchAuditResponse` assertion lets a malformed body
 * (schema drift, an interposed proxy) flow straight into reactive state and
 * throw once the template dereferences a missing field. The first version of
 * this guard (`isSearchAuditResponse`, issue #3111 MEDIUM) over-corrected:
 * it rejected the ENTIRE array when even one record failed validation,
 * hiding every known-good row behind an all-or-nothing "audit unavailable"
 * just because one row (or a future third `SearchAuditRecord` variant) was
 * unrecognized. A single bad row is not evidence the WHOLE response is
 * untrustworthy — only a broken top-level shape (not an object, or
 * `search_audit` isn't even an array to iterate) is.
 * What: returns `null` only for a top-level shape failure (not an object, or
 * `search_audit` missing/non-array — nothing to filter). Otherwise returns
 * `{records, omittedCount}`: `records` is `search_audit` filtered to entries
 * passing {@link isValidRecord} (in original order — filtering never
 * reorders), `omittedCount` is how many entries were dropped. An empty
 * `search_audit` array is valid input and returns `{records: [], omittedCount: 0}`,
 * not `null` — there is a real, empty list, not a shape failure.
 * Test: `search-audit.test.ts::parseSearchAuditResponse`.
 */
export function parseSearchAuditResponse(body: unknown): ParsedSearchAudit | null {
  if (typeof body !== 'object' || body === null) return null;
  const b = body as Record<string, unknown>;
  if (!Array.isArray(b.search_audit)) return null;

  const records: SearchAuditRecord[] = [];
  let omittedCount = 0;
  for (const item of b.search_audit) {
    if (isValidRecord(item)) {
      records.push(item);
    } else {
      omittedCount++;
    }
  }
  return { records, omittedCount };
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
