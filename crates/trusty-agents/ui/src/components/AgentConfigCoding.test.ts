// Why (#4353, DOC-62): this pane's failure mode is a decorative selector — one
// that echoes the user's own choice while the system runs something else. DOC-62
// §5.4 makes a caller-supplied style a ceiling REQUEST the callee may raise, and
// SM-9 currently raises `vibe` to `engineer` unconditionally, so "shows what I
// picked" and "shows what will run" disagree on every setting today. OQ-6's
// answer — expose the override, render the EFFECTIVE style and its resolution
// path — is what these tests pin, along with the two facts about the lane that
// an operator would otherwise have to guess: it is not gated by
// `[subagents].allowed`, and the target name is served, not hardcoded.
// What: mounts the component with `SubagentCoding` wire shapes, exactly as
// `AgentConfigSubagents.test.ts` mounts its own.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import AgentConfigCoding from './AgentConfigCoding.svelte';
import type { ExecutionStyle, StyleResolution, SubagentCoding } from '../lib/agentConfig';

let target: HTMLDivElement;
let instance: Record<string, unknown> | null = null;

/** The resolution every request produces TODAY: floor `vibe`, SM-9 raises it. */
function resolvedToday(requested: ExecutionStyle | null): StyleResolution {
  if (requested === null) {
    return { requested: null, source: 'built-in', effective: 'engineer', escalations: [] };
  }
  const escalations: StyleResolution['escalations'] =
    requested === 'hack'
      ? ['callee-floor', 'tier-unimplemented']
      : requested === 'vibe'
        ? ['tier-unimplemented']
        : [];
  return { requested, source: 'caller', effective: 'engineer', escalations };
}

function coding(over: Partial<SubagentCoding> = {}): SubagentCoding {
  return {
    mechanism: 'coding',
    tool: 'dispatch_task',
    tool_granted: true,
    target: 'coding-pm',
    gated_by_allowed: false,
    lane_floor: 'vibe',
    built_in_default: 'engineer',
    config_default: null,
    resolutions: ([null, 'hack', 'vibe', 'engineer'] as const).map((caller) => ({
      caller,
      resolution: resolvedToday(caller),
    })),
    ...over,
  };
}

/** Rendered prose with whitespace runs collapsed — see the sibling pane's test. */
const copy = () => (target.textContent ?? '').replace(/\s+/g, ' ');

/** The radio for one selector row (`null` = the no-override row). */
function radio(style: ExecutionStyle | 'none'): HTMLInputElement {
  const inputs = Array.from(
    target.querySelectorAll<HTMLInputElement>('input[type="radio"]'),
  );
  const found = inputs.find((i) => (i.value || 'none') === style);
  if (!found) throw new Error(`no radio for ${style}: ${inputs.map((i) => i.value)}`);
  return found;
}

function render(data: SubagentCoding | undefined) {
  instance = mount(AgentConfigCoding, { target, props: { data } }) as unknown as Record<
    string,
    unknown
  >;
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

describe('AgentConfigCoding — the lane', () => {
  it('names the served target rather than a hardcoded literal', () => {
    // A renamed reserved constant must show up here, not silently diverge.
    render(coding({ target: 'renamed-pm' }));
    expect(copy()).toContain('renamed-pm');
  });

  // The fact an operator is most likely to get backwards: every OTHER
  // cross-product target IS allow-list gated, so "add coding-pm to
  // [subagents].allowed" is the natural — and wrong — next step.
  it('says [subagents].allowed does not gate the coding lane', () => {
    render(coding());
    expect(copy()).toContain('[subagents].allowed');
    expect(copy()).toContain('does not gate it');
  });

  it('reports grant state, and says the styles are hypothetical when ungranted', () => {
    render(coding({ tool_granted: false }));
    expect(copy()).toContain('not granted');
    expect(copy()).toContain('does not hold');
    expect(copy()).toContain('if it could');
  });

  it('states that no coding sub-agent is reachable (decision 2)', () => {
    render(coding());
    expect(copy()).toContain('No coding sub-agent is reachable');
  });

  it('renders nothing invented when an older sidecar omits the lane', () => {
    render(undefined);
    expect(copy()).toContain('does not report the coding delegation lane');
    // Above all: no fabricated target, no fabricated resolution.
    expect(copy()).not.toContain('Effective style');
  });
});

describe('AgentConfigCoding — effective vs requested (DOC-62 §3.4, OQ-6)', () => {
  // The load-bearing assertion. The default row must show what RUNS, not what
  // was configured — and with no override and no config default that is the
  // built-in, reported as such.
  it('renders the effective style, not the selection, on first paint', () => {
    render(coding());
    expect(copy()).toContain('Effective style: Engineer — full loop');
    expect(copy()).toContain('the built-in default');
  });

  it('shows a raised style as raised, with the requested value beside it', () => {
    render(coding());
    flushSync(() => radio('vibe').click());
    const text = copy();
    // Both values are present and distinguishable — the request is not hidden,
    // and it is not presented as what happened.
    expect(text).toContain('requested vibe');
    expect(text).toContain('effective engineer');
    expect(text).toContain('raised');
    expect(text).toContain('Ceremony may be raised, never lowered');
  });

  // SM-9 specifically: the user must be told the tier does not exist yet,
  // rather than left to read `vibe → engineer` as a malfunction.
  it('explains the vibe → engineer fail-safe as specified behaviour', () => {
    render(coding());
    flushSync(() => radio('vibe').click());
    expect(copy()).toContain('no implementation yet');
    expect(copy()).toContain('#2596');
  });

  // `hack` is raised TWICE (lane floor, then unimplemented tier) and both
  // reasons must be shown — reporting only one would understate how far the
  // request was moved.
  it('reports every escalation when more than one applied', () => {
    render(coding());
    flushSync(() => radio('hack').click());
    const text = copy();
    expect(text).toContain('coding lane enforces a higher floor');
    expect(text).toContain('no implementation yet');
  });

  it('does not claim a raise when the request was honoured as-is', () => {
    render(coding());
    flushSync(() => radio('engineer').click());
    const text = copy();
    expect(text).toContain('as requested');
    expect(text).not.toContain('Ceremony may be raised, never lowered');
  });

  it('attributes the value to the config default when one is declared', () => {
    render(
      coding({
        config_default: 'vibe',
        resolutions: [
          {
            caller: null,
            resolution: {
              requested: 'vibe',
              source: 'config',
              effective: 'engineer',
              escalations: ['tier-unimplemented'],
            },
          },
        ],
      }),
    );
    expect(copy()).toContain('default_style');
    expect(copy()).toContain('requested vibe');
    expect(copy()).toContain('effective engineer');
  });
});

describe('AgentConfigCoding — the selector', () => {
  it('offers one control per server-reported style plus a no-override row', () => {
    render(coding());
    expect(target.querySelectorAll('input[type="radio"]')).toHaveLength(4);
    expect(copy()).toContain('No override');
  });

  // OQ-3 again, at the render site this time: the control's own label must not
  // read as "delegate to the engineer agent".
  it('labels the engineer STYLE unambiguously', () => {
    render(coding());
    expect(copy()).toContain('Engineer — full loop');
    expect(copy()).toContain('not the tcode "engineer" sub-agent');
  });

  it('says the style is a ceiling and never relaxes a repository gate', () => {
    render(coding());
    expect(copy()).toContain('ceiling request');
    expect(copy()).toContain('never relaxes a check');
    expect(copy()).toContain('grants no capability');
  });

  it('states the precedence order and that the default is edited in agent.toml', () => {
    render(coding());
    expect(copy()).toContain('Read-only here');
    expect(copy()).toContain('agent.toml');
    expect(copy()).toContain('not declared');
  });
});
