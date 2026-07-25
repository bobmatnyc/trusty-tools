// Why (#3894): two regressions this component has already shipped twice are
// pinned here. (1) The instructions editor keeps shrinking back to a strip —
// #3862 fixed a 2-line field with a `55vh/70vh` clamp, which the full-pane
// takeover then made wrong in the other direction (dead space below on a tall
// window). The fix is "grow into whatever the pane gives you", which is a
// structural property: a `flex-1`/`min-h-0` chain with no fixed `rows` and no
// second scroll container wrapped around it. (2) The takeover needs exit
// affordances that actually call back out. Neither is expressible without
// mounting the component.
// What: Mounts the real panel with `fetch` stubbed for the three routes it
// loads (`/api/agents/:name`, `.../persona`, `.../stores`), then asserts the
// sizing invariants and the Back/Close wiring. jsdom does no layout, so the
// "grows with the viewport" half is asserted as the flex chain that produces
// it rather than as a measured pixel height.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount } from 'svelte';
import AgentConfigPanel from './AgentConfigPanel.svelte';

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

const PERSONA = 'You are Izzie.\n'.repeat(300);

function stubApi() {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      const json = url.includes('/persona')
        ? { content: PERSONA, editable: true }
        : url.includes('/stores')
          ? { stores: [{ name: 'izzie-kb', tree: 'okg://izzie', index: 'izzie', connected: true }], issues: [] }
          : {
              name: 'izzie',
              display_name: 'Izzie',
              tools_allow: ['gworkspace_*'],
              scopes: ['agents.read'],
            };
      return { ok: true, status: 200, json: async () => json } as Response;
    }),
  );
}

function mountPanel(onExit = vi.fn()) {
  instance = mount(AgentConfigPanel, {
    target,
    props: { agentName: 'izzie', isConcierge: false, displayName: 'Izzie', onExit },
  }) as unknown as Record<string, unknown>;
  return onExit;
}

const editor = () => target.querySelector('[data-instructions-editor]') as HTMLTextAreaElement;
const tabButton = (label: string) =>
  Array.from(target.querySelectorAll('[role="tab"]')).find(
    (b) => b.textContent?.trim() === label,
  ) as HTMLButtonElement;

beforeEach(() => {
  stubApi();
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

describe('AgentConfigPanel — exit affordances (#3894)', () => {
  it('Back and Close both call onExit', async () => {
    const onExit = mountPanel();
    await waitFor(() => editor() !== null);

    const back = Array.from(target.querySelectorAll('button')).find((b) =>
      b.textContent?.includes('Back to chat'),
    ) as HTMLButtonElement;
    back.click();
    expect(onExit).toHaveBeenCalledTimes(1);

    (target.querySelector('[aria-label="Close agent configuration"]') as HTMLButtonElement).click();
    expect(onExit).toHaveBeenCalledTimes(2);
  });

  it('names the agent with the label the picker used, not the raw id', async () => {
    mountPanel();
    await waitFor(() => editor() !== null);
    expect(target.querySelector('header')?.textContent).toContain('Izzie');
  });
});

describe('AgentConfigPanel — instructions editor sizing (#3894)', () => {
  it('grows into the pane instead of being clamped to a fixed or viewport height', async () => {
    mountPanel();
    await waitFor(() => editor() !== null);

    const classes = editor().className;
    // Grows with whatever height the takeover gives the pane…
    expect(classes).toContain('flex-1');
    // …with `min-h-0`, without which a flex child refuses to shrink below its
    // content and the whole column overflows (the #3862 failure mode).
    expect(classes).toContain('min-h-0');
    // No fixed row count and no viewport clamp: both cap the editor
    // independently of the space actually available.
    expect(editor().hasAttribute('rows')).toBe(false);
    expect(classes).not.toMatch(/min-h-\[\d+vh\]/);
    expect(classes).not.toMatch(/max-h-\[\d+vh\]/);
    // Monospace, per the directive — these are system prompts, not prose.
    expect(classes).toContain('font-mono');
  });

  it('is the only scroll container in its tab — no nested double scrollbars', async () => {
    mountPanel();
    await waitFor(() => editor() !== null);

    expect(editor().className).toContain('overflow-y-auto');

    // Walk from the editor up to the panel root: every ancestor must be a
    // height-passing flex box, never a second scroller wrapped around the
    // first.
    const root = target.firstElementChild as HTMLElement;
    for (let el = editor().parentElement; el && el !== root; el = el.parentElement) {
      expect(el.className).not.toContain('overflow-y-auto');
      expect(el.className).toContain('min-h-0');
      expect(el.className).toContain('flex-1');
    }
    expect(root.className).toContain('overflow-hidden');
  });

  it('the whole persona loads into the editor (multi-hundred-line prompts)', async () => {
    mountPanel();
    await waitFor(() => (editor()?.value.length ?? 0) > 0);
    expect(editor().value).toBe(PERSONA);
    expect(editor().value.split('\n').length).toBeGreaterThan(300);
  });
});

describe('AgentConfigPanel — existing sections still render (#3826/#3816 regression guard)', () => {
  it('keeps the OKG stores, Tools, Permissions and Listeners tabs', async () => {
    mountPanel();
    await waitFor(() => editor() !== null);

    tabButton('OKG Stores').click();
    await waitFor(() => target.textContent?.includes('izzie-kb') ?? false);
    expect(target.textContent).toContain('okg://izzie');

    tabButton('Tools').click();
    await waitFor(() => target.querySelector('textarea') !== null);
    expect((target.querySelector('textarea') as HTMLTextAreaElement).value).toBe('gworkspace_*');

    tabButton('Permissions').click();
    await waitFor(() => target.textContent?.includes('agents.read') ?? false);

    tabButton('Listeners').click();
    await waitFor(() => target.textContent?.includes('Gmail') ?? false);
    expect(target.textContent).toContain('Google Calendar');
  });
});
