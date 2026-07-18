// Why: DOC-39 §8.1 calls `.statusbar` nesting inside `.body` a regression
// that has "bit us twice in the wireframe" and explicitly asks for a
// DOM/structural test (AC-18.1) — this is that test, pinned at the shell
// level so a future refactor of `App.svelte` cannot silently reintroduce
// the nesting. A second, lower-stakes assertion pins that `SessionMonitor`
// (the 8b card, DOC-39 §4.6, refs #2983) mounts inside `.body` — a cheap
// regression guard, not a new normative rule beyond what the spec already
// requires of `.statusbar`.
// What: Mounts the real `App.svelte` (with `fetch` stubbed so the mount
// doesn't make a real network call) and asserts `.statusbar` is a direct
// child of `.app` and NOT a descendant of `.body`, and that the session
// monitor card renders inside `.body`.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount } from 'svelte';
import App from './App.svelte';

let target: HTMLDivElement;
let instance: Record<string, unknown> | null = null;

beforeEach(() => {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue({ ok: false, status: 503, json: async () => ({}) }),
  );
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

describe('App shell structure (DOC-39 §8.1, AC-18.1)', () => {
  it('.statusbar is a sibling of .body, never a descendant', () => {
    instance = mount(App, { target }) as unknown as Record<string, unknown>;

    const app = target.querySelector('.app');
    const body = target.querySelector('.body');
    const statusbar = target.querySelector('.statusbar');

    expect(app).not.toBeNull();
    expect(body).not.toBeNull();
    expect(statusbar).not.toBeNull();
    expect(statusbar!.parentElement).toBe(app);
    expect(body!.contains(statusbar)).toBe(false);
  });

  it('SessionMonitor (8b card, DOC-39 §4.6) renders inside .body', () => {
    instance = mount(App, { target }) as unknown as Record<string, unknown>;

    const body = target.querySelector('.body');
    const headings = Array.from(body?.querySelectorAll('h2') ?? []);

    expect(body).not.toBeNull();
    expect(headings.some((h) => h.textContent === 'session monitor')).toBe(true);
  });
});
