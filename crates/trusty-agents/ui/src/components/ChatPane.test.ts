// Why (#3894): Bob's takeover directive carries a hard constraint that only a
// mounted DOM can express — "leaving config returns to chat exactly as it
// was". The tempting implementation ({#if configOpen} config {:else} chat
// {/if}) satisfies the visible half of the directive and silently breaks this
// half: the chat scrollback and the user's half-typed message are rebuilt from
// scratch on exit. These tests pin the structural property that prevents that
// regression — the chat subtree is the SAME DOM node before, during, and after
// the takeover — plus both exit affordances (Esc and Back).
// What: Mounts the real `ChatPane` with `fetch` stubbed (`ChatHeader` and the
// config panel both fetch on mount), mirroring
// `crates/trusty-code-gui/ui/src/components/ProjectPickerModal.test.ts`'s
// stub-fetch mounting pattern.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { mount, unmount } from 'svelte';
import ChatPane from './ChatPane.svelte';
import { closeConfigPane, configPaneOpen } from '../stores/configPane';

let target: HTMLDivElement;
let instance: Record<string, unknown> | null = null;

async function waitFor(predicate: () => boolean, timeoutMs = 2000): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error('timed out waiting for condition');
}

/** Answers every route the chat pane + config panel touch on mount. */
function stubApi() {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      const json = url.includes('/persona')
        ? { content: 'You are a careful assistant.', editable: true }
        : url.includes('/stores')
          ? { stores: [], issues: [] }
          : url.endsWith('/api/agents')
            ? { agents: [{ name: 'izzie', display_name: 'Izzie' }] }
            : {
                name: 'ctrl',
                display_name: 'Concierge',
                tools_allow: [],
                scopes: [],
              };
      return { ok: true, status: 200, json: async () => json } as Response;
    }),
  );
}

const chatSurface = () =>
  target.querySelector('[data-chat-surface]') as HTMLElement & { inert?: boolean };

/** `inert` is one of Svelte's DOM_PROPERTIES — it is assigned as the IDL
 * property (`node.inert = true`), which is what actually disables the subtree
 * in a browser, rather than as a content attribute. */
const chatIsInert = () => chatSurface().inert === true;
const takeover = () => target.querySelector('[data-config-takeover]');
const gearButton = () => target.querySelector('[aria-label="Configure agent"]') as HTMLButtonElement;
const composer = () => target.querySelector('footer textarea') as HTMLTextAreaElement;

/** Types into the composer the way a user would (Svelte binds on `input`). */
function typeInComposer(text: string) {
  const el = composer();
  el.value = text;
  el.dispatchEvent(new Event('input', { bubbles: true }));
}

function pressEscape() {
  window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
}

beforeEach(() => {
  closeConfigPane();
  stubApi();
  target = document.createElement('div');
  document.body.appendChild(target);
  instance = mount(ChatPane, { target }) as unknown as Record<string, unknown>;
});

afterEach(() => {
  if (instance) {
    unmount(instance);
    instance = null;
  }
  target.remove();
  vi.unstubAllGlobals();
  closeConfigPane();
});

describe('ChatPane — agent configuration takeover (#3894)', () => {
  it('shows chat and no config surface by default', () => {
    expect(chatSurface()).not.toBeNull();
    expect(takeover()).toBeNull();
    expect(chatIsInert()).toBe(false);
  });

  it('the gear button opens a full-pane takeover that covers the chat', async () => {
    gearButton().click();
    await waitFor(() => takeover() !== null);

    const overlay = takeover() as HTMLElement;
    // Covers the whole chat area — absolutely positioned over its (relative)
    // parent, not a strip stacked inside the chat column (#3826's layout).
    expect(overlay.className).toContain('absolute');
    expect(overlay.className).toContain('inset-0');
    // The chat is a SIBLING of the takeover, not an ancestor or descendant.
    expect(overlay.contains(chatSurface())).toBe(false);
    expect(chatSurface().contains(overlay)).toBe(false);
  });

  it('the covered chat stays mounted — same DOM node, still carrying the draft message', async () => {
    typeInComposer('half-written thought');
    const chatNodeBefore = chatSurface();

    gearButton().click();
    await waitFor(() => takeover() !== null);

    // The single most important assertion in this file: the chat subtree was
    // covered, not rebuilt. A same-identity node keeps its scroll offset and
    // every child component's state (including the composer's draft) for free.
    expect(chatSurface()).toBe(chatNodeBefore);
    expect(document.body.contains(chatNodeBefore)).toBe(true);
    expect(composer().value).toBe('half-written thought');
    // …and is inert while covered, so nothing behind the takeover is
    // focusable or exposed to assistive tech.
    expect(chatIsInert()).toBe(true);
  });

  it('Esc exits the takeover and returns to the untouched chat', async () => {
    typeInComposer('draft that must survive');
    const chatNodeBefore = chatSurface();

    gearButton().click();
    await waitFor(() => takeover() !== null);

    pressEscape();
    await waitFor(() => takeover() === null);

    expect(get(configPaneOpen)).toBe(false);
    expect(chatSurface()).toBe(chatNodeBefore);
    expect(chatIsInert()).toBe(false);
    expect(composer().value).toBe('draft that must survive');
  });

  it('the Back button exits the takeover', async () => {
    gearButton().click();
    await waitFor(() => takeover() !== null);

    const back = Array.from(target.querySelectorAll('button')).find((b) =>
      b.textContent?.includes('Back to chat'),
    ) as HTMLButtonElement;
    back.click();
    await waitFor(() => takeover() === null);

    expect(get(configPaneOpen)).toBe(false);
    expect(chatSurface()).not.toBeNull();
  });

  it('the close button exits the takeover', async () => {
    gearButton().click();
    await waitFor(() => takeover() !== null);

    (target.querySelector('[aria-label="Close agent configuration"]') as HTMLButtonElement).click();
    await waitFor(() => takeover() === null);

    expect(get(configPaneOpen)).toBe(false);
  });

  it('the gear reflects the takeover state via aria-pressed', async () => {
    expect(gearButton().getAttribute('aria-pressed')).toBe('false');

    gearButton().click();
    await waitFor(() => takeover() !== null);
    expect(gearButton().getAttribute('aria-pressed')).toBe('true');

    // NB: the gear cannot be the way BACK — it sits inside the `inert` chat
    // while the takeover is up, so a real browser ignores clicks on it. That
    // is exactly why the takeover carries its own Back/Close/Esc exits.
    pressEscape();
    await waitFor(() => takeover() === null);
    expect(gearButton().getAttribute('aria-pressed')).toBe('false');
  });
});
