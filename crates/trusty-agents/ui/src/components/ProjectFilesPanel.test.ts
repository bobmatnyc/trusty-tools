// Why (#4359): this panel is where "only markdown is editable" stops being a
// predicate and becomes something a user can see, so the regressions worth
// pinning are the ones a passing type check would miss: a non-markdown entry
// must be listed but NOT openable (the surface #4360 replaces with a viewer),
// an unreachable file route must surface the daemon's message rather than an
// empty directory (#4357 has not landed — a silent empty listing would read as
// "this project has no docs"), and opening a markdown file must actually put
// its contents in the editor.
// What: mounts the panel against a stubbed `fetch` and drives it through list
// → open → dirty → save.
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
  it('lists every entry but only lets markdown be opened (#4360 seam)', async () => {
    stubRoutes(() => [200, { entries: [README, REPORT] }]);
    render();
    await settle();
    expect(target.textContent).toContain('README.md');
    expect(target.textContent).toContain('report.pdf');
    expect(buttonFor('README.md').disabled).toBe(false);
    expect(buttonFor('report.pdf').disabled).toBe(true);
    expect(buttonFor('report.pdf').title).toContain('Only markdown files');
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
