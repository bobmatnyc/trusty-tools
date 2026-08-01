// Why (#4360): this panel is where the document-type table stops being a
// lookup and becomes something a user can see, so the regressions worth
// pinning are the ones a passing type check would miss: each of the three
// kinds must present differently (editable opens; view-only is listed, named
// and refused; unsupported is refused without pretending a viewer is coming),
// an unreachable file route must surface the daemon's message rather than an
// empty directory (#4357 has not landed — a silent empty listing would read as
// "this project has no docs"), and opening an editable file must actually put
// its contents in a writable editor.
// What: mounts the panel against a stubbed `fetch` and drives it through list
// → open → dirty → save, plus the three-way row gating.
// Test: this file.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount } from 'svelte';
import ProjectFilesPanel from './ProjectFilesPanel.svelte';

let target: HTMLDivElement;
let instance: Record<string, unknown> | null = null;

const README = {
  name: 'README.md',
  path: 'README.md',
  is_dir: false,
  size: 6,
  modified: null,
};
const REPORT = {
  name: 'report.pdf',
  path: 'report.pdf',
  is_dir: false,
  size: 99,
  modified: null,
};
/** Not in the table at all — the third row state. */
const SOURCE = {
  name: 'main.rs',
  path: 'src/main.rs',
  is_dir: false,
  size: 42,
  modified: null,
};

/**
 * Route-aware `fetch` stub — the panel makes a sequence of calls, so a single
 * canned response cannot express list-then-read.
 */
function stubRoutes(handler: (url: string, init?: RequestInit) => [number, unknown]) {
  const fn = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const [status, body] = handler(String(input), init);
    return {
      ok: status >= 200 && status < 300,
      status,
      text: async () => JSON.stringify(body),
    };
  });
  vi.stubGlobal('fetch', fn);
  return fn;
}

/** Let the panel's chained awaits and Svelte's flush settle. */
async function settle() {
  for (let i = 0; i < 6; i += 1) await Promise.resolve();
  await new Promise((resolve) => setTimeout(resolve, 0));
}

/**
 * Wait for a condition instead of a fixed number of ticks — opening a document
 * chains a fetch AND the editor's dynamic `import()`, and the number of
 * microtasks that takes is a bundler detail no test should encode.
 */
async function waitFor(what: string, predicate: () => boolean, ticks = 100) {
  for (let i = 0; i < ticks; i += 1) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
  throw new Error(`timed out waiting for ${what}`);
}

function render() {
  instance = mount(ProjectFilesPanel, {
    target,
    props: { projectId: 'trusty-tools' },
  }) as unknown as Record<string, unknown>;
}

function buttonFor(label: string): HTMLButtonElement {
  const match = [...target.querySelectorAll('button')].find((b) =>
    (b.textContent ?? '').includes(label),
  );
  if (!match) throw new Error(`no button matching ${label}`);
  return match as HTMLButtonElement;
}

beforeEach(() => {
  target = document.createElement('div');
  document.body.appendChild(target);
});

afterEach(() => {
  if (instance) {
    unmount(instance);
    instance = null;
  }
  target.remove();
  vi.unstubAllGlobals();
});

describe('ProjectFilesPanel — listing', () => {
  it('lists every entry but only lets markdown be opened', async () => {
    stubRoutes(() => [200, { entries: [README, REPORT, SOURCE] }]);
    render();
    await settle();
    expect(target.textContent).toContain('README.md');
    expect(target.textContent).toContain('report.pdf');
    expect(target.textContent).toContain('main.rs');
    expect(buttonFor('README.md').disabled).toBe(false);
    expect(buttonFor('report.pdf').disabled).toBe(true);
    expect(buttonFor('main.rs').disabled).toBe(true);
  });

  it('tells the two refused kinds apart instead of one blanket message', async () => {
    stubRoutes(() => [200, { entries: [README, REPORT, SOURCE] }]);
    render();
    await settle();

    // View-only: named, and visibly marked rather than tooltip-only, because a
    // known document type is a promise that a viewer is coming (#4401).
    const pdf = buttonFor('report.pdf');
    expect(pdf.title).toContain('PDF documents are view-only');
    expect(pdf.textContent).toContain('view-only');

    // Unsupported: refused outright, and must NOT read as view-only.
    const rs = buttonFor('main.rs');
    expect(rs.title).toContain('not a supported document type');
    expect(rs.textContent).toContain('unsupported');
    expect(rs.textContent).not.toContain('view-only');

    // Editable rows carry no gating badge at all.
    expect(buttonFor('README.md').textContent).not.toContain('view-only');
    expect(buttonFor('README.md').textContent).not.toContain('unsupported');
  });

  it('never reads a view-only document into the editor', async () => {
    // End-to-end statement of the gate: activating the row must not put a PDF
    // in the editor. (`open()` re-checks the table itself, which the disabled
    // attribute means this click cannot reach — that guard is defence in depth
    // for #4401 mounting documents by a path other than a row click.)
    const fetchMock = stubRoutes(() => [200, { entries: [REPORT] }]);
    render();
    await settle();
    buttonFor('report.pdf').click();
    await settle();
    expect(fetchMock.mock.calls.some(([url]) => String(url).includes('/files/read'))).toBe(
      false,
    );
    expect(target.querySelector('.cm-content')).toBeNull();
  });

  it('surfaces a failing file route instead of an empty directory', async () => {
    stubRoutes(() => [404, { error: 'files/list not found' }]);
    render();
    await settle();
    expect(target.textContent).toContain('Could not list project files');
    expect(target.textContent).toContain('files/list not found');
    expect(target.textContent).not.toContain('No files in this directory');
  });
});

describe('ProjectFilesPanel — open and save', () => {
  it('loads markdown into the editor and writes edits back', async () => {
    const fetchMock = stubRoutes((url) => {
      if (url.includes('/files/list')) return [200, { entries: [README] }];
      if (url.includes('/files/read')) return [200, { path: 'README.md', content: '# hello' }];
      return [200, {}];
    });
    render();
    await settle();
    buttonFor('README.md').click();
    await waitFor('the editor to mount', () => target.querySelector('.cm-content') !== null);

    expect(target.querySelector('.cm-content')?.textContent).toContain('# hello');
    // The `readonly` prop is derived from the table, so an editable document
    // must arrive writable — if the derivation inverted, this is what breaks.
    expect(target.querySelector('.cm-content')?.getAttribute('contenteditable')).toBe('true');
    // Nothing edited yet, so Save has nothing to do.
    expect(buttonFor('Save').disabled).toBe(true);

    // Type into the real editor rather than poking component state — the
    // change → dirty → enabled-Save chain is the behaviour under test.
    const { EditorView } = await import('@codemirror/view');
    const view = EditorView.findFromDOM(target as HTMLElement);
    view?.dispatch({ changes: { from: view.state.doc.length, insert: ' world' } });
    await settle();
    expect(buttonFor('Save').disabled).toBe(false);

    buttonFor('Save').click();
    await settle();
    const write = fetchMock.mock.calls.find(([url]) => String(url).includes('/files/write'));
    expect(write).toBeDefined();
    expect(JSON.parse(String((write?.[1] as RequestInit).body))).toEqual({
      path: 'README.md',
      content: '# hello world',
    });
    expect(buttonFor('Save').disabled).toBe(true);
  });
});
