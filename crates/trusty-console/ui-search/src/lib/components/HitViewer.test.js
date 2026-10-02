/*
 * Why: the hit viewer is the search dashboard's consumer of the shared
 * Foundry `CodeView` (docs/design/UI/design-system/components/), imported
 * through the `@foundry` alias rather than copied. A broken alias or a
 * gutter that does not start at the hit's first line would only show in a
 * browser.
 * What: mounts the real HitViewer on a search hit and checks the dialog, the
 * line-numbered gutter, the syntax highlighting, and Escape closing it.
 * Test: this file.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';

import HitViewer from './HitViewer.svelte';

let target = null;
let instance = null;

afterEach(() => {
  if (instance) unmount(instance);
  instance = null;
  target?.remove();
  target = null;
});

const hit = {
  id: '/repo/src/lib.rs:10:12',
  file: '/repo/src/lib.rs',
  path: 'src/lib.rs',
  index_id: 'repo',
  start_line: 10,
  end_line: 12,
  language: 'rust',
  function_name: 'parse',
  content: 'fn parse() {\n    let n = 1;\n}\n'
};

function render() {
  target = document.createElement('div');
  document.body.appendChild(target);
  const onclose = vi.fn();
  instance = mount(HitViewer, { target, props: { hit, onclose } });
  flushSync();
  return onclose;
}

describe('HitViewer', () => {
  it("renders the hit's line range, numbered from its first line and highlighted", async () => {
    render();
    const dialog = target.querySelector('[role="dialog"]');
    expect(dialog.getAttribute('aria-modal')).toBe('true');
    expect(document.getElementById(dialog.getAttribute('aria-labelledby')).textContent).toBe('src/lib.rs');
    expect(dialog.textContent).toContain('Lines 10–12');
    expect(target.querySelector('.gutter').textContent).toBe('10\n11\n12');
    const source = target.querySelector('pre[aria-label="File source"]');
    expect(source.textContent).toBe(hit.content); // plain text until the grammar chunk loads
    await vi.waitFor(() => {
      flushSync();
      expect(source.querySelector('.hljs-keyword')?.textContent).toBe('fn');
    });
    expect(source.textContent).toBe(hit.content);
    expect(document.activeElement).toBe(target.querySelector('button[aria-label="Close file viewer"]'));
  });

  it('keeps Tab and Shift+Tab inside the dialog', () => {
    render();
    const close = target.querySelector('button[aria-label="Close file viewer"]');
    const region = target.querySelector('[role="region"]');
    const tab = (shiftKey) =>
      window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', shiftKey, cancelable: true }));

    region.focus();
    tab(false); // past the last focusable element -> wraps to the first
    expect(document.activeElement).toBe(close);
    tab(true); // before the first -> wraps to the last
    expect(document.activeElement).toBe(region);

    const outside = document.createElement('button');
    document.body.appendChild(outside);
    outside.focus();
    tab(false); // focus that escaped the dialog is pulled back in
    expect(document.activeElement).toBe(close);
    outside.remove();
  });

  it('closes on Escape', () => {
    const onclose = render();
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', cancelable: true }));
    expect(onclose).toHaveBeenCalledTimes(1);
  });
});
