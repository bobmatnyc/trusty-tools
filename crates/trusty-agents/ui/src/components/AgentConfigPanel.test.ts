// Why (#3894/#3932): three properties of this shell have each shipped wrong at
// least once, and none is expressible without mounting the component.
// (1) The instructions editor keeps shrinking back to a strip — #3862 fixed a
// 2-line field with a `55vh/70vh` clamp, which the full-pane takeover then made
// wrong in the other direction (dead space below on a tall window). The fix is
// "grow into whatever the pane gives you", a structural property: a
// `flex-1`/`min-h-0` chain with no fixed `rows` and no second scroll container
// wrapped around it — which the #3932 decomposition into per-section child
// components must not break (DOC-57 §8.4, G-6b / C-07.3).
// (2) The takeover needs exit affordances that actually call back out, and the
// persona edit buffer must survive a SECTION SWITCH now that the body it lives
// in is an unmounted-on-switch child (G-6a / C-07.4).
// (3) The five sections and their order are normative (DOC-57 §2.1, C-07.1) —
// #4029 adds Sub-agents as a SIXTH section without reordering any of the five,
// which is itself asserted below — and every pane must render an explicit
// empty/error state rather than fabricated content when its backend is absent
// or failing (G-4, C-07.2).
// What: Mounts the real panel with `fetch` stubbed for the routes it
// loads (`/api/agents/:name`, `.../persona`, `.../stores`, `.../skills`,
// `.../subagents`), then asserts the
// sizing invariants, the Back/Close wiring, the tab strip, and each pane's
// degraded rendering. jsdom does no layout, so the "grows with the viewport"
// half is asserted as the flex chain that produces it rather than as a measured
// pixel height (`tests/config-takeover.spec.ts` measures the real thing).
// Test: this file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, tick, unmount } from 'svelte';
import AgentConfigPanel from './AgentConfigPanel.svelte';

/**
 * The C-07.2 "degraded backends" test below asserts that a disabled MCP
 * knowledge connection renders honestly as DISABLED rather than being hidden.
 * That guarantee must hold regardless of which endpoints the shipped
 * `config.toml` defaults currently happen to enable — trusty-memory and
 * trusty-search moved from disabled OpenRPC stubs to enabled live MCP-stdio
 * services (#4064), which correctly left zero disabled entries in the real
 * `KNOWLEDGE_MCP_ENDPOINTS` list. Rather than re-coupling this regression test
 * to whatever is enabled-by-default today (which would just break again next
 * time that list changes), append one synthetic always-disabled endpoint so
 * the "disabled renders as disabled" invariant is tested independently of the
 * shipped defaults.
 */
vi.mock('../lib/agentConfig', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../lib/agentConfig')>();
  return {
    ...actual,
    KNOWLEDGE_MCP_ENDPOINTS: [
      ...actual.KNOWLEDGE_MCP_ENDPOINTS,
      {
        name: 'stub-disabled-endpoint',
        description: 'Synthetic endpoint fixture — always disabled (regression coverage only).',
        enabled: false,
        scopes: ['stub.read'],
        reason: 'disabled in this test fixture, independent of shipped config.toml defaults',
      },
    ],
  };
});

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

/**
 * `GET /api/agents/:name/skills` (#3933) — one skill per tool, human-named.
 * Mirrors the real route's shape so the pane test exercises the same parse the
 * sidecar feeds it.
 */
const SKILLS_PAYLOAD = {
  skills: [
    {
      id: 'git-history',
      name: 'Git Commit History',
      description: 'Read recent commits with author, date and subject.',
      kind: 'action',
      origin: { kind: 'builtin' },
      granted: true,
      tools: ['git_log'],
      provider: null,
    },
    {
      id: 'knowledge-search',
      name: 'Knowledge Search',
      description: "Search this agent's bound knowledge store.",
      kind: 'knowledge',
      origin: { kind: 'builtin' },
      granted: true,
      tools: ['vector_search'],
      provider: null,
    },
    {
      id: 'gmail-search',
      name: 'Gmail Search',
      description: 'Find messages in Gmail using Gmail query syntax.',
      kind: 'action',
      origin: { kind: 'builtin' },
      granted: false,
      tools: ['search_gmail_messages'],
      provider: {
        provider: 'Google Workspace',
        requirement: 'An authorized Google account profile.',
        env_var: null,
        configured: null,
      },
    },
  ],
  granted_count: 2,
  unresolved: [],
  unmatched_patterns: [
    { pattern: 'gworkspace_*', reason: 'may resolve to an MCP tool at dispatch time' },
  ],
  dead_scope_patterns: [],
  declares_capability: true,
};

/**
 * `GET /api/agents/:name/subagents` (#4029) — the UNION of both delegation
 * mechanisms, kept apart by kind. Mirrors the real route's shape, including a
 * tier-blocked target so the panel test exercises the gate's rendering too.
 */
const SUBAGENTS_PAYLOAD = {
  resolved: true,
  in_product: {
    mechanism: 'in_product',
    tool: 'delegate_to_agent',
    tool_registered: true,
    tool_granted: true,
    delegator_tier: 'l1',
    allowed_roles: ['engineer', 'qa', 'researcher', 'documentation', 'ops', 'planner', 'assistant'],
    targets: [
      {
        name: 'engineer',
        display_name: 'Engineer',
        role: 'engineer',
        tier: 'l1',
        reachable: true,
        reason: null,
      },
      {
        name: 'orch',
        display_name: 'Orchestrator',
        role: 'assistant',
        tier: 'l0',
        reachable: false,
        reason: 'refused by the L0/L1 delegation gate (#4169)',
      },
    ],
    role_excluded_count: 2,
    hidden_excluded_count: 0,
    unresolved: [],
  },
  cross_product: {
    mechanism: 'cross_product',
    tool: 'dispatch_task',
    tool_granted: true,
    declares_allowed: true,
    bridge_floor: ['research', 'ticketing'],
    targets: [
      { name: 'research', granted: true, reason: null },
      { name: 'ticketing', granted: false, reason: 'not listed in [subagents].allowed' },
    ],
    rejected: [],
  },
};

/** The bare-agent counterpart: everything denied, fail-closed. */
const BARE_SUBAGENTS_PAYLOAD = {
  resolved: true,
  in_product: {
    mechanism: 'in_product',
    tool: 'delegate_to_agent',
    tool_registered: true,
    tool_granted: false,
    delegator_tier: 'l1',
    allowed_roles: ['engineer', 'qa', 'researcher', 'documentation', 'ops', 'planner', 'assistant'],
    targets: [],
    role_excluded_count: 0,
    hidden_excluded_count: 0,
    unresolved: [],
  },
  cross_product: {
    mechanism: 'cross_product',
    tool: 'dispatch_task',
    tool_granted: false,
    declares_allowed: false,
    bridge_floor: ['research', 'ticketing'],
    targets: [
      { name: 'research', granted: false, reason: 'fail-closed' },
      { name: 'ticketing', granted: false, reason: 'fail-closed' },
    ],
    rejected: [],
  },
};

function stubApi() {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      const json = url.includes('/persona')
        ? { content: PERSONA, editable: true }
        : url.includes('/subagents')
        ? SUBAGENTS_PAYLOAD
        : url.includes('/skills')
          ? SKILLS_PAYLOAD
          : url.includes('/stores')
          ? { stores: [{ name: 'izzie-kb', tree: 'okg://izzie', index: 'izzie', connected: true }], issues: [] }
          : {
              name: 'izzie',
              display_name: 'Izzie',
              tools_allow: ['gworkspace_*', 'vector_search', 'git_log', 'git_status'],
              scopes: ['agents.read'],
            };
      return { ok: true, status: 200, json: async () => json } as Response;
    }),
  );
}

/**
 * The C-07.2 fixture: an agent that declares nothing, with the stores route
 * failing outright. Every pane must still render, each saying what it does not
 * know rather than filling the gap.
 */
function stubBareApiWithFailingStores() {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes('/stores')) return { ok: false, status: 503, json: async () => ({}) } as Response;
      const json = url.includes('/persona')
        ? { content: '', editable: false }
        : url.includes('/subagents')
        ? BARE_SUBAGENTS_PAYLOAD
        : url.includes('/skills')
          ? {
              skills: [],
              granted_count: 0,
              unresolved: [],
              unmatched_patterns: [],
              dead_scope_patterns: [],
              declares_capability: false,
            }
          : { name: 'bare', display_name: 'Bare', tools_allow: [], scopes: [] };
      return { ok: true, status: 200, json: async () => json } as Response;
    }),
  );
}

function mountPanel(onExit = vi.fn(), agentName = 'izzie') {
  instance = mount(AgentConfigPanel, {
    target,
    props: { agentName, isConcierge: false, displayName: 'Izzie', onExit },
  }) as unknown as Record<string, unknown>;
  return onExit;
}

const editor = () => target.querySelector('[data-instructions-editor]') as HTMLTextAreaElement;
const tabLabels = () =>
  Array.from(target.querySelectorAll('[role="tab"]')).map((b) => b.textContent?.trim());
const tabButton = (label: string) =>
  Array.from(target.querySelectorAll('[role="tab"]')).find(
    (b) => b.textContent?.trim() === label,
  ) as HTMLButtonElement;

/** Drives `bind:value` the way a keystroke would. */
function typeInto(el: HTMLTextAreaElement, value: string) {
  el.value = value;
  el.dispatchEvent(new Event('input', { bubbles: true }));
}

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

  it('an unsaved persona edit blocks Back and raises the confirm dialog (C-07.4)', async () => {
    const onExit = mountPanel();
    await waitFor(() => editor() !== null);

    typeInto(editor(), 'edited prompt');
    // The child→shell binding propagates synchronously, but the shell's
    // `personalityDirty` is a reactive statement and lands on the next flush.
    // A real keystroke and a real click are always separate tasks; only the
    // test needs to say so.
    await tick();

    const back = Array.from(target.querySelectorAll('button')).find((b) =>
      b.textContent?.includes('Back to chat'),
    ) as HTMLButtonElement;
    back.click();

    await waitFor(() => target.querySelector('[role="alertdialog"]') !== null);
    expect(onExit).not.toHaveBeenCalled();
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
    // first. Splitting the body into child components (#3932) adds no DOM
    // node, so this chain is unchanged — which is exactly what it must prove.
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

describe('AgentConfigPanel — six sections (#3932 + #4029, DOC-57 §8.2)', () => {
  // #4029 (epic #4021, OQ-5 resolved by the owner 2026-07-26) adds Sub-agents
  // as the sixth section. DOC-57 §2.1's ordering stays normative and stays
  // SATISFIED: the five original sections keep their relative order exactly —
  // Permissions is still last, Listeners still after Skills — and the new
  // section is inserted rather than reordering any existing pair. #4182 amends
  // the spec text.
  it('renders exactly six tabs, the five normative ones in order (C-07.1)', async () => {
    mountPanel();
    await waitFor(() => editor() !== null);
    expect(tabLabels()).toEqual([
      'Personality',
      'Knowledge',
      'Skills',
      'Sub-agents',
      'Listeners',
      'Permissions',
    ]);
    // The normative five, with the insertion removed, must be byte-identical to
    // DOC-57 §2.1's order — the property that would break if a future edit
    // "made room" by moving one of them.
    expect(tabLabels().filter((l) => l !== 'Sub-agents')).toEqual([
      'Personality',
      'Knowledge',
      'Skills',
      'Listeners',
      'Permissions',
    ]);
  });

  it('each section renders its own data', async () => {
    mountPanel();
    await waitFor(() => editor() !== null);

    tabButton('Knowledge').click();
    await waitFor(() => target.textContent?.includes('izzie-kb') ?? false);
    expect(target.textContent).toContain('okg://izzie');
    // The vector_search→store seam (§4.3) and the declared MCP endpoints (§4.4).
    expect(target.textContent).toContain('vector_search');
    expect(target.textContent).toContain('trusty-memory');

    // Skills renders RESOLVED capability (#3933): one human-named card per
    // granted tool, kind-grouped, with the ungranted remainder counted rather
    // than listed.
    tabButton('Skills').click();
    await waitFor(() => target.textContent?.includes('Git Commit History') ?? false);
    expect(target.querySelector('textarea')).toBeNull();
    expect(target.textContent).toContain('git_log');
    expect(target.textContent).toContain('Knowledge Search');
    expect(target.textContent).not.toContain('Gmail Search');
    expect(target.textContent).toContain('2 of 3 known skills granted');

    // #4029: both delegation mechanisms, labelled separately, sourced from
    // `GET .../subagents` — never derived client-side from `tools_allow`.
    tabButton('Sub-agents').click();
    await waitFor(() => target.textContent?.includes('delegate_to_agent') ?? false);
    expect(target.textContent).toContain('Engineer');
    expect(target.textContent).toContain('dispatch_task');
    expect(target.textContent).toContain('1 of 2 cross-product specialists granted');

    tabButton('Listeners').click();
    await waitFor(() => target.textContent?.includes('Gmail') ?? false);
    expect(target.textContent).toContain('Google Calendar');

    tabButton('Permissions').click();
    await waitFor(() => target.textContent?.includes('agents.read') ?? false);
    expect(target.textContent).toContain('gworkspace_*');
  });

  it('keeps an in-progress persona edit across a section switch (G-6a)', async () => {
    mountPanel();
    await waitFor(() => editor() !== null);

    typeInto(editor(), 'half-written system prompt');
    tabButton('Skills').click();
    await waitFor(() => editor() === null);

    tabButton('Personality').click();
    await waitFor(() => editor() !== null);
    expect(editor().value).toBe('half-written system prompt');
  });
});

describe('AgentConfigPanel — degraded backends (#3932, C-07.2)', () => {
  it('renders every pane with an explicit empty/error state and no fabricated content', async () => {
    vi.unstubAllGlobals();
    stubBareApiWithFailingStores();
    mountPanel(vi.fn(), 'bare');
    await waitFor(() => tabLabels().length === 6);

    // Personality: no editable persona.md is a stated fact, not a blank box.
    await waitFor(() => target.textContent?.includes('no editable persona.md') ?? false);

    tabButton('Knowledge').click();
    await waitFor(() => target.textContent?.includes('Could not read store bindings') ?? false);
    // The knowledge-classification gap is named rather than guessed at, and a
    // disabled endpoint is shown as disabled rather than hidden (C-03.2). The
    // synthetic `stub-disabled-endpoint` fixture (see the `agentConfig` mock
    // above) supplies the disabled entry independently of whatever the
    // shipped config.toml defaults currently enable.
    expect(target.textContent).toContain('kind = "knowledge"');
    expect(target.textContent).toContain('disabled');

    tabButton('Skills').click();
    await waitFor(() => target.textContent?.includes('declares neither') ?? false);
    // An absent allow-list denies; the pane must not read as "unrestricted".
    expect(target.textContent).toContain('not "unrestricted"');

    // #4029: a bare agent must be told that absent DENIES on both mechanisms —
    // the pane may never read an absent `[subagents]` section as "all".
    tabButton('Sub-agents').click();
    await waitFor(() => target.textContent?.includes('declares no') ?? false);
    expect(target.textContent).toContain('does not mean "all"');
    expect(target.textContent).toContain('0 of 2 cross-product specialists granted');

    tabButton('Listeners').click();
    await waitFor(() => target.textContent?.includes('Scaffolding, not configuration') ?? false);
    // L-1: the hardcoded per-listener "not bound" badge is gone.
    expect(target.textContent).not.toContain('not bound');

    tabButton('Permissions').click();
    await waitFor(() => target.textContent?.includes('No allow-list declared') ?? false);
    expect(target.textContent).toContain('No scopes declared');
    // PM-6: unenforced mechanisms are marked as such.
    expect(target.textContent).toContain('not enforced');
  });
});
