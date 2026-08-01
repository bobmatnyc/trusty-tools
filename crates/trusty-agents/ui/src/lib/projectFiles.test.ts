// Why (#4359): this module is the only place that knows what "editable" means
// in this slice and what the #4357 routes look like, so the two regressions
// worth locking down are the write gate (a non-markdown write must never reach
// the network) and the request shape (path travels as a query parameter, not
// baked into the route, or every path containing `/` breaks). The list
// envelope tolerance matters too — #4357 has not landed, and a client that
// only accepts one of the two obvious JSON shapes fails on merge day.
// What: `isMarkdownPath` classification, both `files/list` envelopes, the URL
// each helper builds, and `writeProjectFile`'s refusal.
// Test: this file.

import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  isMarkdownPath,
  listProjectFiles,
  readProjectFile,
  writeProjectFile,
} from './projectFiles';

afterEach(() => {
  vi.unstubAllGlobals();
});

/** Stub `fetch` with one canned JSON response, capturing the request. */
function stubFetch(status: number, body: unknown) {
  const fn = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => ({
    ok: status >= 200 && status < 300,
    status,
    text: async () => JSON.stringify(body),
  }));
  vi.stubGlobal('fetch', fn);
  return fn;
}

function requestedUrl(fn: ReturnType<typeof stubFetch>): URL {
  return new URL(String(fn.mock.calls[0][0]), 'http://localhost');
}

const ENTRY = {
  name: 'README.md',
  path: 'README.md',
  is_dir: false,
  size: 12,
  modified: '2026-08-01T00:00:00Z',
};

describe('isMarkdownPath', () => {
  it('accepts both markdown extensions, case-insensitively', () => {
    expect(isMarkdownPath('docs/README.md')).toBe(true);
    expect(isMarkdownPath('NOTES.MD')).toBe(true);
    expect(isMarkdownPath('legacy.markdown')).toBe(true);
  });

  it('rejects the document types #4360 will render view-only', () => {
    expect(isMarkdownPath('spec.pdf')).toBe(false);
    expect(isMarkdownPath('sheet.xlsx')).toBe(false);
    expect(isMarkdownPath('notes.txt')).toBe(false);
    // A directory named like a document is still not a markdown file.
    expect(isMarkdownPath('md')).toBe(false);
  });
});

describe('listProjectFiles', () => {
  it('sends the directory as a query parameter on the #4357 route', async () => {
    const fn = stubFetch(200, { entries: [ENTRY] });
    await listProjectFiles('trusty-tools', 'docs/design');
    const url = requestedUrl(fn);
    expect(url.pathname).toBe('/api/projects/trusty-tools/files/list');
    expect(url.searchParams.get('path')).toBe('docs/design');
  });

  it('accepts both a bare array and an {entries} envelope', async () => {
    stubFetch(200, [ENTRY]);
    await expect(listProjectFiles('p')).resolves.toEqual([ENTRY]);
    vi.unstubAllGlobals();
    stubFetch(200, { entries: [ENTRY] });
    await expect(listProjectFiles('p')).resolves.toEqual([ENTRY]);
  });

  it('propagates the daemon error instead of returning an empty listing', async () => {
    stubFetch(404, { error: 'no such project' });
    await expect(listProjectFiles('ghost')).rejects.toThrow('no such project');
  });
});

describe('readProjectFile', () => {
  it('requests files/read with the file path', async () => {
    const fn = stubFetch(200, { path: 'README.md', content: '# hi' });
    await expect(readProjectFile('p', 'README.md')).resolves.toEqual({
      path: 'README.md',
      content: '# hi',
    });
    const url = requestedUrl(fn);
    expect(url.pathname).toBe('/api/projects/p/files/read');
    expect(url.searchParams.get('path')).toBe('README.md');
  });
});

describe('writeProjectFile', () => {
  it('POSTs path + content to files/write', async () => {
    const fn = stubFetch(200, {});
    await writeProjectFile('p', 'docs/a.md', '# body');
    expect(requestedUrl(fn).pathname).toBe('/api/projects/p/files/write');
    const init = fn.mock.calls[0][1] as RequestInit;
    expect(init.method).toBe('POST');
    expect(JSON.parse(String(init.body))).toEqual({
      path: 'docs/a.md',
      content: '# body',
    });
  });

  it('refuses a non-markdown write without touching the network (#4359 gate)', async () => {
    const fn = stubFetch(200, {});
    await expect(writeProjectFile('p', 'report.pdf', 'x')).rejects.toThrow(
      /Only markdown files are editable/,
    );
    expect(fn).not.toHaveBeenCalled();
  });
});
