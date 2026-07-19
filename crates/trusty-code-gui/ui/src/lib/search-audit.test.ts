// Why: `parseSearchAuditResponse` is the runtime shape guard/subset-filter
// `SearchTab.svelte` relies on to never trust a malformed
// `GET /sessions/{id}/search-audit` body — a gap here means a schema-drifted
// daemon response either silently crashes the tab or (the issue #3111 MEDIUM
// this file's coverage was extended for) drops every known-good row just
// because one record was bad. The three label formatters are the only other
// client-side logic in `search-audit.ts` and each has a branch per
// `SearchAuditRecord` variant that must be pinned directly.
// What: covers valid `search`/`recall` records, the top-level shape
// failures `parseSearchAuditResponse` must still reject wholesale (not an
// object, missing/non-array `search_audit`), and the per-record filtering
// behavior: all-valid, some-invalid (subset + omitted count), all-invalid
// (empty subset, non-zero count, still not `null`), and an unrecognized
// future `kind` tolerated as one more omitted row rather than a fatal
// rejection. Plus the label formatters' per-variant/`null`-field output.
// Test: this file.
import { describe, expect, it } from 'vitest';
import {
  auditHitsLabel,
  auditLaneLabel,
  auditLatencyLabel,
  parseSearchAuditResponse,
  type SearchAuditRecallRecord,
  type SearchAuditSearchRecord,
} from './search-audit';

const searchRecord: SearchAuditSearchRecord = {
  kind: 'search',
  agent: 'engineer',
  agent_id: 'ag-1',
  lane: 'semantic',
  query: 'parse config',
  hit_count: 7,
  latency_ms: 42,
  at: '2026-07-18T00:00:00Z',
};

const recallRecord: SearchAuditRecallRecord = {
  kind: 'recall',
  agent: 'pm',
  agent_id: 'ag-2',
  query: 'prior decisions',
  result_count: 5,
  injected_count: 3,
  at: '2026-07-18T00:00:01Z',
};

describe('parseSearchAuditResponse', () => {
  it('accepts a response with valid search and recall records (all-valid)', () => {
    const parsed = parseSearchAuditResponse({ search_audit: [searchRecord, recallRecord] });
    expect(parsed).toEqual({ records: [searchRecord, recallRecord], omittedCount: 0 });
  });

  it('accepts an empty search_audit array (no activity yet), not a shape failure', () => {
    expect(parseSearchAuditResponse({ search_audit: [] })).toEqual({
      records: [],
      omittedCount: 0,
    });
  });

  it('accepts a search record with hit_count: null', () => {
    const record = { ...searchRecord, hit_count: null };
    expect(parseSearchAuditResponse({ search_audit: [record] })).toEqual({
      records: [record],
      omittedCount: 0,
    });
  });

  it('returns null for non-object bodies (top-level shape failure)', () => {
    expect(parseSearchAuditResponse(null)).toBeNull();
    expect(parseSearchAuditResponse('ok')).toBeNull();
    expect(parseSearchAuditResponse([searchRecord])).toBeNull();
  });

  it('returns null for a body missing search_audit (top-level shape failure)', () => {
    expect(parseSearchAuditResponse({})).toBeNull();
  });

  it('returns null for a body whose search_audit is not an array (top-level shape failure)', () => {
    expect(parseSearchAuditResponse({ search_audit: 'oops' })).toBeNull();
  });

  it('some-invalid: filters to the valid subset and reports the omitted count, never rejecting the whole response', () => {
    const malformed = { ...searchRecord, lane: 42 }; // lane must be a string
    const parsed = parseSearchAuditResponse({
      search_audit: [searchRecord, malformed, recallRecord],
    });
    expect(parsed).toEqual({ records: [searchRecord, recallRecord], omittedCount: 1 });
  });

  it('all-invalid: returns an empty subset with a non-zero omitted count, still not null', () => {
    const parsed = parseSearchAuditResponse({
      search_audit: [
        { ...searchRecord, latency_ms: 'not-a-number' },
        { ...recallRecord, result_count: 'five' },
      ],
    });
    expect(parsed).not.toBeNull();
    expect(parsed).toEqual({ records: [], omittedCount: 2 });
  });

  it('tolerates an unrecognized future kind as one omitted row, not a fatal rejection', () => {
    const futureRecord = {
      kind: 'graph_traverse',
      agent: 'engineer',
      agent_id: 'ag-3',
      query: 'call graph',
      at: '2026-07-18T00:00:02Z',
    };
    const parsed = parseSearchAuditResponse({
      search_audit: [searchRecord, futureRecord],
    });
    expect(parsed).toEqual({ records: [searchRecord], omittedCount: 1 });
  });

  it('rejects a record missing a common field (agent) as one omitted row', () => {
    const { agent: _agent, ...rest } = searchRecord;
    const parsed = parseSearchAuditResponse({ search_audit: [rest, recallRecord] });
    expect(parsed).toEqual({ records: [recallRecord], omittedCount: 1 });
  });

  it('preserves original ordering when filtering out invalid entries', () => {
    const malformed = { ...recallRecord, injected_count: 'oops' };
    const parsed = parseSearchAuditResponse({
      search_audit: [searchRecord, malformed, recallRecord],
    });
    expect(parsed?.records.map((r) => r.agent_id)).toEqual(['ag-1', 'ag-2']);
  });
});

describe('auditLaneLabel', () => {
  it("renders the real lane for a search record", () => {
    expect(auditLaneLabel(searchRecord)).toBe('semantic');
  });

  it("renders 'recall' for a recall record (no lane on the wire)", () => {
    expect(auditLaneLabel(recallRecord)).toBe('recall');
  });
});

describe('auditHitsLabel', () => {
  it('renders the hit count for a search record', () => {
    expect(auditHitsLabel(searchRecord)).toBe('7');
  });

  it('renders an em dash for a null hit_count, never a fabricated 0', () => {
    expect(auditHitsLabel({ ...searchRecord, hit_count: null })).toBe('—');
  });

  it('renders result_count with injected_count for a recall record', () => {
    expect(auditHitsLabel(recallRecord)).toBe('5 (3 injected)');
  });
});

describe('auditLatencyLabel', () => {
  it('renders milliseconds for a search record', () => {
    expect(auditLatencyLabel(searchRecord)).toBe('42ms');
  });

  it('renders an em dash for a recall record (no latency on the wire)', () => {
    expect(auditLatencyLabel(recallRecord)).toBe('—');
  });
});
