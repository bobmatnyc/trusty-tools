// Why: `isSearchAuditResponse` is the runtime shape guard `SearchTab.svelte`
// relies on to never trust a malformed `GET /sessions/{id}/search-audit`
// body — a gap here means a schema-drifted daemon response silently crashes
// the tab instead of degrading. The three label formatters are the only
// other client-side logic in `search-audit.ts` and each has a branch per
// `SearchAuditRecord` variant that must be pinned directly.
// What: covers valid `search`/`recall` records, every malformed-shape
// rejection `isSearchAuditResponse` must catch, and the label formatters'
// per-variant/`null`-field output.
// Test: this file.
import { describe, expect, it } from 'vitest';
import {
  auditHitsLabel,
  auditLaneLabel,
  auditLatencyLabel,
  isSearchAuditResponse,
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

describe('isSearchAuditResponse', () => {
  it('accepts a response with valid search and recall records', () => {
    expect(isSearchAuditResponse({ search_audit: [searchRecord, recallRecord] })).toBe(true);
  });

  it('accepts an empty search_audit array (no activity yet)', () => {
    expect(isSearchAuditResponse({ search_audit: [] })).toBe(true);
  });

  it('accepts a search record with hit_count: null', () => {
    expect(
      isSearchAuditResponse({ search_audit: [{ ...searchRecord, hit_count: null }] }),
    ).toBe(true);
  });

  it('rejects non-object bodies', () => {
    expect(isSearchAuditResponse(null)).toBe(false);
    expect(isSearchAuditResponse('ok')).toBe(false);
    expect(isSearchAuditResponse([searchRecord])).toBe(false);
  });

  it('rejects a body missing search_audit', () => {
    expect(isSearchAuditResponse({})).toBe(false);
  });

  it('rejects a body whose search_audit is not an array', () => {
    expect(isSearchAuditResponse({ search_audit: 'oops' })).toBe(false);
  });

  it('rejects a record with an unknown kind', () => {
    expect(
      isSearchAuditResponse({ search_audit: [{ ...searchRecord, kind: 'other' }] }),
    ).toBe(false);
  });

  it('rejects a record missing a common field (agent)', () => {
    const { agent: _agent, ...rest } = searchRecord;
    expect(isSearchAuditResponse({ search_audit: [rest] })).toBe(false);
  });

  it('rejects a search record with a non-string lane', () => {
    expect(
      isSearchAuditResponse({ search_audit: [{ ...searchRecord, lane: 42 }] }),
    ).toBe(false);
  });

  it('rejects a search record with a non-numeric latency_ms', () => {
    expect(
      isSearchAuditResponse({ search_audit: [{ ...searchRecord, latency_ms: '42' }] }),
    ).toBe(false);
  });

  it('rejects a recall record with a non-numeric result_count', () => {
    expect(
      isSearchAuditResponse({
        search_audit: [{ ...recallRecord, result_count: 'five' }],
      }),
    ).toBe(false);
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
