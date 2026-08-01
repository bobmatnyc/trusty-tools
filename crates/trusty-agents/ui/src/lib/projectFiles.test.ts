// Why (#4360): this module is the only place that knows what a file may do
// here and what the #4357 routes look like, so the regressions worth locking
// down are the gate (a non-editable write must never reach the network), the
// three-way classification the UI reads, and the request shape (path travels
// as a query parameter, not baked into the route, or every path containing `/`
// breaks). The list envelope tolerance matters too — #4357 has not landed, and
// a client that only accepts one of the two obvious JSON shapes fails on merge
// day. The table is asserted as a whole because "which types are allowed" is
// the deliverable, so silently gaining or losing a row should fail here.
// What: `DOCUMENT_TYPES` and its three-way classification, both `files/list`
// envelopes, the URL each helper builds, and `writeProjectFile`'s refusal.
// Test: this file.

import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  DOCUMENT_TYPES,
  documentKindFor,
  documentTypeFor,
  editRefusalReason,
  isEditablePath,
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

describe('DOCUMENT_TYPES', () => {
  it('enumerates exactly the agreed set, and only markdown is editable', () => {
    expect(DOCUMENT_TYPES.map((t) => t.extension)).toEqual([
      '.md',
      '.markdown',
      '.pdf',
      '.docx',
      '.xlsx',
      '.xls',
      '.txt',
      '.csv',
    ]);
    expect(DOCUMENT_TYPES.filter((t) => t.kind === 'editable').map((t) => t.extension)).toEqual([
      '.md',
      '.markdown',
    ]);
  });

  it('leaves the types still awaiting an owner call unsupported', () => {
    // Absent from the table means refused — the safe default. Adding any of
    // these is an owner decision (#4360), not a silent widening.
    for (const path of ['deck.pptx', 'diagram.png', 'photo.jpg', 'notes.rst', 'plan.org']) {
      expect(documentKindFor(path)).toBe('unsupported');
    }
  });
});

describe('documentKindFor', () => {
  it('classifies markdown as editable, case-insensitively', () => {
    expect(documentKindFor('docs/README.md')).toBe('editable');
    expect(documentKindFor('NOTES.MD')).toBe('editable');
    expect(documentKindFor('legacy.markdown')).toBe('editable');
    expect(isEditablePath('docs/README.md')).toBe(true);
  });

  it('classifies the extractable and plain-text formats as view-only', () => {
    for (const path of ['spec.pdf', 'brief.docx', 'sheet.xlsx', 'old.xls', 'a.txt', 'b.csv']) {
      expect(documentKindFor(path)).toBe('view-only');
      expect(isEditablePath(path)).toBe(false);
    }
  });

  it('refuses anything the table does not claim', () => {
    expect(documentKindFor('main.rs')).toBe('unsupported');
    // A file with no extension is not a document.
    expect(documentKindFor('md')).toBe('unsupported');
    expect(documentKindFor('LICENSE')).toBe('unsupported');
    // A dotfile's leading dot is not an extension.
    expect(documentKindFor('.gitignore')).toBe('unsupported');
  });

  it('reads the extension off the base name, not the whole path', () => {
    // A dotted directory must not lend its suffix to an extensionless file.
    expect(documentKindFor('docs/v1.md/NOTES')).toBe('unsupported');
    expect(documentKindFor('docs/v1.2/README.md')).toBe('editable');
  });

  it('does not confuse extensions that are prefixes of one another', () => {
    // `.xlsx`.endsWith('.xls') would be a false match under naive suffix logic.
    expect(documentTypeFor('sheet.xlsx')?.label).toBe('Excel workbook');
    expect(documentTypeFor('notes.markdown')?.extension).toBe('.markdown');
  });
});

describe('editRefusalReason', () => {
  it('says nothing about an editable document', () => {
    expect(editRefusalReason('README.md')).toBeNull();
  });

  it('names the type for a view-only document and the gap for an unknown one', () => {
    expect(editRefusalReason('spec.pdf')).toContain('PDF documents are view-only');
    expect(editRefusalReason('main.rs')).toContain('.rs is not a supported document type');
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

  it('refuses a view-only write without touching the network (#4360 gate)', async () => {
    const fn = stubFetch(200, {});
    await expect(writeProjectFile('p', 'report.pdf', 'x')).rejects.toThrow(
      /PDF documents are view-only/,
    );
    expect(fn).not.toHaveBeenCalled();
  });

  it('refuses an unsupported write without touching the network', async () => {
    const fn = stubFetch(200, {});
    await expect(writeProjectFile('p', 'src/main.rs', 'x')).rejects.toThrow(
      /not a supported document type/,
    );
    expect(fn).not.toHaveBeenCalled();
  });

  it('gates the write on the same table the UI reads', async () => {
    // The guard must not carry its own list: every view-only and unsupported
    // type is refused, and every editable one is attempted. A second list
    // would let the panel offer what the client then rejects.
    const fn = stubFetch(200, {});
    for (const type of DOCUMENT_TYPES) {
      const path = `doc${type.extension}`;
      if (type.kind === 'editable') {
        await expect(writeProjectFile('p', path, 'x')).resolves.toBeUndefined();
      } else {
        await expect(writeProjectFile('p', path, 'x')).rejects.toThrow(/view-only/);
      }
    }
    expect(fn).toHaveBeenCalledTimes(
      DOCUMENT_TYPES.filter((t) => t.kind === 'editable').length,
    );
  });
});
