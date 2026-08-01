// Why (#4359): the editor is a controlled component wrapping a stateful
// CodeMirror view, and the two ways that goes wrong are both invisible to a
// type checker: the document not tracking an externally-changed `value`, and
// the `readonly` prop not actually reaching the view (which is exactly the
// lever #4360 flips to make non-markdown documents view-only — a prop that
// renders but does not enforce would ship a "view-only" document you can type
// into). The controlled-sync guard also has to survive a parent that echoes
// every `change` straight back, the normal wiring for this component.
// What: mounts the real component in jsdom and asserts document text tracks
// the prop, edits dispatch `change`, an echoing parent does not clobber the
// document, and `readonly` reaches the CodeMirror content element.
// Test: this file.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import { EditorView } from '@codemirror/view';
import MarkdownEditor from './MarkdownEditor.svelte';
import MarkdownEditorHost from './__fixtures__/MarkdownEditorHost.svelte';

let target: HTMLDivElement;
let instance: Record<string, unknown> | null = null;

function render(
  props: Record<string, unknown> = {},
  events: Record<string, (e: CustomEvent) => void> = {},
) {
  instance = mount(MarkdownEditor, {
    target,
    props: { value: '', ...props },
    events,
  } as never) as unknown as Record<string, unknown>;
  // `onMount` (where the CodeMirror view is created) runs in a microtask;
  // flush it so every assertion below sees a mounted editor.
  flushSync();
  return instance;
}

/**
 * Mount the editor under a reactive parent — the only way to change a prop
 * after mount, since `mount()`'s props object is not reactive on its own.
 */
function renderHost(props: { initialValue?: string; initialReadonly?: boolean }) {
  const host = mount(MarkdownEditorHost, { target, props }) as unknown as {
    setValue: (v: string) => void;
    setReadonly: (v: boolean) => void;
  };
  instance = host as unknown as Record<string, unknown>;
  flushSync();
  return host;
}

/** The live CodeMirror view, reached the way an integration would reach it. */
function view(): EditorView {
  const found = EditorView.findFromDOM(target);
  if (!found) throw new Error('CodeMirror view not mounted');
  return found;
}

/** The contenteditable CodeMirror renders into the host. */
function content(): HTMLElement {
  const el = target.querySelector('.cm-content');
  if (!el) throw new Error('CodeMirror content element not mounted');
  return el as HTMLElement;
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
});

describe('MarkdownEditor — mounting', () => {
  it('renders the initial value into the document', () => {
    render({ value: '# Heading\n\nbody' });
    expect(content().textContent).toContain('# Heading');
    expect(content().textContent).toContain('body');
  });

  it('labels the editing region for screen readers', () => {
    render({ value: '', ariaLabel: 'Markdown editor for docs/a.md' });
    expect(content().getAttribute('aria-label')).toBe('Markdown editor for docs/a.md');
  });
});

describe('MarkdownEditor — change events', () => {
  it('dispatches change with the full document on every edit', () => {
    const onChange = vi.fn();
    render({ value: 'a' }, { change: onChange });
    view().dispatch({ changes: { from: 1, insert: 'b' } });
    expect(onChange).toHaveBeenCalledTimes(1);
    expect((onChange.mock.calls[0][0] as CustomEvent).detail).toEqual({ value: 'ab' });
  });

  it('does not dispatch change for a selection-only transaction', () => {
    const onChange = vi.fn();
    render({ value: 'abc' }, { change: onChange });
    view().dispatch({ selection: { anchor: 2 } });
    expect(onChange).not.toHaveBeenCalled();
  });
});

describe('MarkdownEditor — controlled value', () => {
  it('tracks an externally changed value', () => {
    const host = renderHost({ initialValue: 'first' });
    host.setValue('second');
    flushSync();
    expect(content().textContent).toContain('second');
    expect(content().textContent).not.toContain('first');
  });

  it('leaves the document and cursor alone when the parent echoes an edit back', () => {
    const host = renderHost({ initialValue: 'a' });
    view().dispatch({ changes: { from: 1, insert: 'b' }, selection: { anchor: 2 } });
    // What a real parent does: take the `change` payload and feed it back in.
    host.setValue('ab');
    flushSync();
    expect(content().textContent).toBe('ab');
    // Re-inserting the whole document would collapse the cursor to 0.
    expect(view().state.selection.main.head).toBe(2);
  });
});

describe('MarkdownEditor — readonly (#4360 gate)', () => {
  it('marks the surface non-editable when readonly', () => {
    render({ value: 'locked', readonly: true });
    expect(content().getAttribute('contenteditable')).toBe('false');
  });

  it('is editable by default', () => {
    render({ value: 'open' });
    expect(content().getAttribute('contenteditable')).toBe('true');
  });

  it('reconfigures the live view when readonly flips', () => {
    const host = renderHost({ initialValue: 'open' });
    host.setReadonly(true);
    flushSync();
    expect(content().getAttribute('contenteditable')).toBe('false');
    expect(view().state.readOnly).toBe(true);
  });
});
