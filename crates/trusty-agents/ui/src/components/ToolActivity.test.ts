import { afterEach, describe, expect, it } from 'vitest';
import { mount, unmount } from 'svelte';
import ToolActivity from './ToolActivity.svelte';
let instance: ReturnType<typeof mount> | undefined;
afterEach(async () => { if (instance) await unmount(instance); instance = undefined; document.body.innerHTML = ''; });
describe('compact tool activity', () => {
  it('shows a compact row without an expander when details are unavailable', () => {
    instance = mount(ToolActivity, { target: document.body, props: { name: 'Search files' } });
    expect(document.body.textContent).toContain('Search files');
    expect(document.querySelector('[data-tool-activity] svg')).not.toBeNull();
    expect(document.querySelector('details')).toBeNull();
  });
  it('offers collapsed native disclosure and keeps tool details as text', () => {
    instance = mount(ToolActivity, { target: document.body, props: { name: 'Read file', details: '<script>unsafe()</script>\nfile contents', status: 'running' } });
    const details = document.querySelector('details')!;
    expect(details.open).toBe(false);
    expect(document.querySelector('summary')?.textContent).toContain('Read file');
    expect(document.querySelector('pre')?.textContent).toContain('<script>unsafe()</script>');
    expect(document.querySelector('script')).toBeNull();
    expect(document.querySelector('[aria-label="Running"]')).not.toBeNull();
    details.open = true;
    expect(details.open).toBe(true);
  });
});

it('humanizes escaped tool identifiers while retaining the exact identifier', () => {
  instance = mount(ToolActivity, { target: document.body, props: { name: 'granola\\_search' } });
  const label = document.querySelector('.name');
  expect(label?.textContent).toBe('Granola search');
  expect(label?.getAttribute('title')).toBe('granola\\_search');
});
