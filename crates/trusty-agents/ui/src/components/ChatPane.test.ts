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
import { closeConfigPane, configPaneDirty, configPaneOpen } from '../stores/configPane';
import { activeAgentId, activeProjectId, addMessage } from '../stores/app';

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
  configPaneDirty.set(false);
  activeAgentId.set(null);
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

    // The gear does close a CLEAN panel (it routes through the exit guard,
    // which finds nothing to lose) — though in a real browser it also sits
    // inside the `inert` chat, so the takeover's own Back/Close/Esc are the
    // reachable exits.
    pressEscape();
    await waitFor(() => takeover() === null);
    expect(gearButton().getAttribute('aria-pressed')).toBe('false');
  });
});

// ---------------------------------------------------------------------------
// PR #3895 code-critic follow-ups. Each block pins one HIGH finding.
// ---------------------------------------------------------------------------

const editor = () =>
  target.querySelector('[data-instructions-editor]') as HTMLTextAreaElement | null;
const confirmDialog = () => target.querySelector('[role="alertdialog"]');
const button = (label: string) =>
  Array.from(target.querySelectorAll('button')).find((b) =>
    b.textContent?.trim().includes(label),
  ) as HTMLButtonElement;

/** Opens the takeover and dirties the Personality editor. */
async function openAndDirty(text = 'a half-finished system prompt') {
  gearButton().click();
  await waitFor(() => (editor()?.value.length ?? 0) > 0);
  const el = editor() as HTMLTextAreaElement;
  el.value = text;
  el.dispatchEvent(new Event('input', { bubbles: true }));
  await waitFor(() => get(configPaneDirty));
}

describe('ChatPane — unsaved edits are never discarded silently (HIGH-1)', () => {
  it('Esc on a dirty panel confirms instead of closing', async () => {
    await openAndDirty();

    pressEscape();
    await waitFor(() => confirmDialog() !== null);

    expect(takeover()).not.toBeNull();
    expect(get(configPaneOpen)).toBe(true);
    expect(editor()?.value).toBe('a half-finished system prompt');
  });

  it('Back and Close on a dirty panel confirm instead of closing', async () => {
    await openAndDirty();

    button('Back to chat').click();
    await waitFor(() => confirmDialog() !== null);
    expect(get(configPaneOpen)).toBe(true);

    button('Keep editing').click();
    await waitFor(() => confirmDialog() === null);

    (target.querySelector('[aria-label="Close agent configuration"]') as HTMLButtonElement).click();
    await waitFor(() => confirmDialog() !== null);
    expect(get(configPaneOpen)).toBe(true);
  });

  it('"Keep editing" returns to the panel with the edit intact', async () => {
    await openAndDirty();
    pressEscape();
    await waitFor(() => confirmDialog() !== null);

    button('Keep editing').click();
    await waitFor(() => confirmDialog() === null);

    expect(takeover()).not.toBeNull();
    expect(editor()?.value).toBe('a half-finished system prompt');
  });

  it('"Discard changes" is the only path that throws the edit away', async () => {
    await openAndDirty();
    pressEscape();
    await waitFor(() => confirmDialog() !== null);

    button('Discard changes').click();
    await waitFor(() => takeover() === null);

    expect(get(configPaneOpen)).toBe(false);
    expect(get(configPaneDirty)).toBe(false);
  });

  it('"Save and close" persists the edit before leaving', async () => {
    await openAndDirty('saved before exit');
    pressEscape();
    await waitFor(() => confirmDialog() !== null);

    button('Save and close').click();
    await waitFor(() => takeover() === null);

    const patch = (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls.find(
      (c) => (c[1] as RequestInit | undefined)?.method === 'PATCH',
    );
    expect(patch).toBeDefined();
    expect(JSON.parse(String((patch![1] as RequestInit).body))).toEqual({
      personality: 'saved before exit',
    });
    expect(get(configPaneOpen)).toBe(false);
  });

  it('the gear cannot silently discard a dirty panel', async () => {
    // PR #3895 re-review: the gear is an exit affordance too. It used to bind a
    // raw toggle, so a second press closed a dirty panel with no confirm — its
    // only protection was `inert` on the covered chat, which is a rendering
    // property, not a data-loss guard (and unverified in Tauri's WKWebView).
    await openAndDirty('typed, then the gear was pressed again');

    gearButton().click();
    await waitFor(() => confirmDialog() !== null);

    expect(takeover()).not.toBeNull();
    expect(get(configPaneOpen)).toBe(true);

    button('Keep editing').click();
    await waitFor(() => confirmDialog() === null);
    expect(takeover()).not.toBeNull();
    expect(editor()?.value).toBe('typed, then the gear was pressed again');
  });

  it('a clean panel still exits on the first press — the guard is not a nag', async () => {
    gearButton().click();
    await waitFor(() => takeover() !== null);

    pressEscape();
    await waitFor(() => takeover() === null);
    expect(confirmDialog()).toBeNull();
  });
});

describe('ChatPane — the configured agent is pinned at open time (HIGH-2)', () => {
  it('a background agent switch cannot retarget the open panel or discard its edits', async () => {
    await openAndDirty('edits for the pinned agent');
    expect(target.querySelector('header')?.textContent).toContain('Concierge');

    // The Sidebar's resume-workstream flow does exactly this while the
    // takeover is up — it lives outside the inert chat.
    activeAgentId.set('izzie');
    await new Promise((r) => setTimeout(r, 20));

    expect(target.querySelector('header')?.textContent).toContain('Concierge');
    expect(editor()?.value).toBe('edits for the pinned agent');
    // No refetch was triggered for the other agent.
    const calls = (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls.map((c) => String(c[0]));
    expect(calls.some((u) => u.includes('/api/agents/izzie'))).toBe(false);
  });
});

describe('ChatPane — a covered chat keeps its scroll offset (HIGH-3)', () => {
  it('a message arriving while the takeover is open leaves the scrolled-up chat where it was', async () => {
    const scroller = target.querySelector('[data-chat-scroll]') as HTMLElement;
    // jsdom does no layout, so give the container real metrics: 1000px of
    // content in a 200px window, parked 100px from the top (i.e. scrolled up
    // to read history — decidedly NOT pinned to the bottom).
    Object.defineProperty(scroller, 'scrollHeight', { value: 1000, configurable: true });
    Object.defineProperty(scroller, 'clientHeight', { value: 200, configurable: true });
    Object.defineProperty(scroller, 'scrollTop', { value: 100, writable: true, configurable: true });
    scroller.dispatchEvent(new Event('scroll'));

    gearButton().click();
    await waitFor(() => takeover() !== null);

    // An SSE delta landing mid-configuration.
    addMessage(get(activeProjectId), {
      id: 'm-while-covered',
      role: 'assistant',
      content: 'a reply that arrived while you were configuring',
      timestamp: Date.now(),
    });
    await new Promise((r) => setTimeout(r, 20));

    expect(scroller.scrollTop).toBe(100);

    pressEscape();
    await waitFor(() => takeover() === null);
    expect(scroller.scrollTop).toBe(100);
  });
});
