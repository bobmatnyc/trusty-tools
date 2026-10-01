/*
 * Why: suggestions come from every index at once; the merge and the
 * error-propagation rules decide what the operator sees and whether the
 * search box gives up on a server with no typeahead route.
 * What: covers `mergeSuggestions` ranking/dedupe and `fanOutTypeahead`'s
 * tagging, partial-failure tolerance, and all-failed rethrow.
 * Test: this file.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';

vi.mock('./api.js', () => ({ api: { typeahead: vi.fn() } }));

import { api } from './api.js';
import { fanOutTypeahead, mergeSuggestions } from './typeahead.js';

afterEach(() => vi.clearAllMocks());

describe('typeahead fan-out', () => {
  it('mergeSuggestions ranks across indexes and drops duplicates', () => {
    const a = { index_id: 'a', path: 'x.rs', start_line: 1, score: 1 };
    const b = { index_id: 'b', path: 'y.rs', start_line: 2, score: 5 };
    expect(mergeSuggestions([[a, { ...a }], [b]])).toEqual([b, a]);
    expect(mergeSuggestions([[a], [b]], 1)).toEqual([b]);
  });

  it('fanOutTypeahead tags hits, skips a failing index, and rethrows when every index fails', async () => {
    api.typeahead.mockImplementation(async (id) => {
      if (id === 'bad') throw Object.assign(new Error('boom'), { status: 500 });
      return { hits: [{ label: id, path: `${id}.rs`, start_line: 1, score: id === 'two' ? 2 : 1 }] };
    });
    const signal = new AbortController().signal;
    const hits = await fanOutTypeahead(['one', 'bad', 'two'], 'q', signal);
    expect(hits.map((h) => [h.index_id, h.label])).toEqual([['two', 'two'], ['one', 'one']]);

    api.typeahead.mockRejectedValue(Object.assign(new Error('501'), { status: 501 }));
    await expect(fanOutTypeahead(['one', 'two'], 'q', signal)).rejects.toMatchObject({ status: 501 });
  });
});
