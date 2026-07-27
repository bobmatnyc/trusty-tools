// Why (#3932, DOC-57 §4): the Knowledge pane widened the old OKG Stores tab
// from one sub-surface to three, and the spec's conformance criteria are about
// what it must NOT do: never collapse loading into empty (G-4's three states),
// never omit a disabled knowledge endpoint (C-03.2), and never guess which
// capabilities are knowledge-classified when no manifest declares `kind` yet
// (§4.3). The one linkage it MUST show is `vector_search` under the store it
// reads (§4.3, the #3864 seam).
// What: Mounts the component directly with props — the shell owns the fetches.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { mount, unmount } from 'svelte';
import AgentConfigKnowledge from './AgentConfigKnowledge.svelte';

let target: HTMLDivElement;
let instance: Record<string, unknown> | null = null;

const STORE = {
  name: 'izzie-kb',
  tree: 'okg://izzie',
  index: 'izzie-kb',
  connected: true,
  chunk_count: 552,
};

function render(props: Record<string, unknown> = {}) {
  instance = mount(AgentConfigKnowledge, {
    target,
    props: { stores: [STORE], issues: [], error: '', storeBoundTools: [], ...props },
  }) as unknown as Record<string, unknown>;
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

describe('AgentConfigKnowledge — store bindings (K-a)', () => {
  it('distinguishes loading from empty from error (G-4)', () => {
    render({ stores: null });
    expect(target.textContent).toContain('Resolving stores…');
    unmount(instance!);

    render({ stores: [] });
    expect(target.textContent).toContain('binds no OKG store');
    unmount(instance!);

    render({ stores: [], error: 'GET /api/agents/izzie/stores failed: 503' });
    expect(target.textContent).toContain('Could not read store bindings');
    expect(target.textContent).not.toContain('binds no OKG store');
  });

  it('renders a resolved binding with its live stats', () => {
    render();
    expect(target.textContent).toContain('izzie-kb');
    expect(target.textContent).toContain('okg://izzie');
    expect(target.textContent).toContain('552');
    expect(target.textContent).toContain('connected');
  });
});

describe('AgentConfigKnowledge — knowledge tools (K-b)', () => {
  it('shows the store-bound tool under the store it reads (§4.3)', () => {
    render({ storeBoundTools: ['vector_search'] });
    expect(target.textContent).toContain('reads via');
    expect(target.textContent).toContain('reading');
    expect(target.textContent).toContain('vector_search');
  });

  it('names the classification gap rather than guessing a knowledge tool set', () => {
    render({ storeBoundTools: [] });
    expect(target.textContent).toContain('grants no store-bound knowledge tool');
    expect(target.textContent).toContain('kind = "knowledge"');
  });

  // PR #3945 code-critic HIGH-1. `storeBoundTools` is derived from
  // `[tools].allow` alone, so before the fix the pane asserted a linkage from
  // the GRANT without consulting whether any binding existed — rendering
  // "binds no OKG store" and "bound to the store above" in the same view. Two
  // independent facts must be correlated before either is claimed.
  it('never claims a store linkage for an agent that binds nothing', () => {
    render({ stores: [], storeBoundTools: ['vector_search'] });
    expect(target.textContent).toContain('binds no OKG store');
    // The contradicting half must be absent in every spelling it could take.
    expect(target.textContent).not.toContain('the store above');
    expect(target.textContent).not.toContain('reads via');
    expect(target.textContent).not.toMatch(/reading\s/);
    // …replaced by what actually happens: no agent-owned store, so the tool
    // uses its own shipped index.
    expect(target.textContent).toContain('binds no');
    expect(target.textContent).toContain("shipped default index");
  });

  it('claims no linkage while the bindings are still resolving', () => {
    render({ stores: null, storeBoundTools: ['vector_search'] });
    expect(target.textContent).toContain('resolving which store it reads');
    expect(target.textContent).not.toContain('reads via');
    expect(target.textContent).not.toContain('shipped default index');
  });

  it('points a multi-store agent at the one binding the tool actually defaults to', () => {
    render({
      stores: [STORE, { ...STORE, name: 'second-kb', index: 'second-kb' }],
      storeBoundTools: ['vector_search'],
    });
    // `default_search_index` is the PRIMARY binding — one chip, on one card,
    // not one per card implying the tool reads all of them.
    expect(target.textContent?.match(/reads via/g)).toHaveLength(1);
    expect(target.textContent).toContain('izzie-kb');
    expect(target.textContent).toContain('Only the first binding is the default search target');
  });
});

describe('AgentConfigKnowledge — MCP connections (K-c)', () => {
  it('lists every declared endpoint, never hidden, with a reason iff disabled (C-03.2)', () => {
    render();
    expect(target.textContent).toContain('trusty-memory');
    expect(target.textContent).toContain('trusty-search');
    expect(target.textContent).toContain('gworkspace');
    // trusty-memory/trusty-search moved from dead `[[tool_registry.endpoints]]`
    // OpenRPC stubs to live `[[mcp.services]]` MCP-stdio entries, so they no
    // longer carry a disabled-reason — asserting the OLD `--rpc` reason text
    // would now be asserting a fabricated status (the exact defect C-03.2
    // guards against), so it must be ABSENT.
    expect(target.textContent).not.toContain('awaits `--rpc` mode');
    // The list is the shipped declaration, not a probe — saying so is the
    // difference between honest scaffolding and an invented status.
    expect(target.textContent).toContain('not probed');
  });
});
