// Why (#4029, epic #4021 OQ-5): this pane is the first surface to render
// delegation reach, and the failure mode that matters is showing a target the
// runtime would refuse. The properties worth pinning are therefore about what it
// must NOT do: never merge the two mechanisms into one list (their target
// vocabularies are disjoint and their enforcement layers differ), never render a
// tier-blocked target as callable, never read an absent `[subagents]` section as
// a grant, never collapse "the tool is registered" into "this agent may call
// it", and never collapse loading into empty (DOC-57 §8.3's G-4 three states).
// What: Mounts the component directly with `AgentSubagents` wire shapes — the
// shell owns the fetch, exactly as `AgentConfigSkills.test.ts` does.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { mount, unmount } from 'svelte';
import AgentConfigSubagents from './AgentConfigSubagents.svelte';
import type {
  AgentSubagents,
  SubagentCrossProduct,
  SubagentInProduct,
  SubagentInProductTarget,
} from '../lib/agentConfig';

let target: HTMLDivElement;
let instance: Record<string, unknown> | null = null;

function inTarget(over: Partial<SubagentInProductTarget> = {}): SubagentInProductTarget {
  return {
    name: 'engineer',
    display_name: 'Engineer',
    role: 'engineer',
    tier: 'l1',
    reachable: true,
    reason: null,
    ...over,
  };
}

function inProduct(over: Partial<SubagentInProduct> = {}): SubagentInProduct {
  return {
    mechanism: 'in_product',
    tool: 'delegate_to_agent',
    tool_registered: true,
    tool_granted: true,
    delegator_tier: 'l1',
    allowed_roles: ['engineer', 'qa', 'researcher', 'documentation', 'ops', 'planner', 'assistant'],
    targets: [inTarget()],
    role_excluded_count: 0,
    unresolved: [],
    ...over,
  };
}

function crossProduct(over: Partial<SubagentCrossProduct> = {}): SubagentCrossProduct {
  return {
    mechanism: 'cross_product',
    tool: 'dispatch_task',
    tool_granted: false,
    declares_allowed: false,
    bridge_floor: ['research', 'ticketing'],
    targets: [
      { name: 'research', granted: false, reason: 'not listed in [subagents].allowed' },
      { name: 'ticketing', granted: false, reason: 'not listed in [subagents].allowed' },
    ],
    rejected: [],
    ...over,
  };
}

function payload(over: Partial<AgentSubagents> = {}): AgentSubagents {
  return {
    resolved: true,
    in_product: inProduct(),
    cross_product: crossProduct(),
    ...over,
  };
}

/** Every element whose own label is exactly the `reachable` badge. */
const reachableBadges = () =>
  Array.from(target.querySelectorAll('span')).filter(
    (s) => s.textContent?.trim() === 'reachable',
  );

function render(data: AgentSubagents | null, error = '') {
  instance = mount(AgentConfigSubagents, {
    target,
    props: { data, error },
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

describe('AgentConfigSubagents — the two mechanisms', () => {
  it('labels both mechanisms separately rather than flattening them', () => {
    render(payload());
    expect(target.textContent).toContain('In-product');
    expect(target.textContent).toContain('delegate_to_agent');
    expect(target.textContent).toContain('Cross-product');
    expect(target.textContent).toContain('dispatch_task');
    // The two sections must be distinct elements, not one merged list.
    expect(target.querySelectorAll('section').length).toBe(2);
  });

  it('states that the in-product target set is not configurable', () => {
    render(payload());
    expect(target.textContent).toContain('role allow-list');
    // The allow-list is quoted verbatim so the operator can see the rule —
    // including `documentation`, the role that exists in NO other allow-set.
    expect(target.textContent).toContain('documentation');
  });

  it('renders a reachable in-product target with its role and tier', () => {
    render(payload());
    expect(target.textContent).toContain('Engineer');
    expect(target.textContent).toContain('engineer');
    expect(target.textContent).toContain('tier l1');
    expect(target.textContent).toContain('reachable');
  });

  it('distinguishes loading from loaded from error (G-4)', () => {
    render(null);
    expect(target.textContent).toContain('Resolving sub-agents…');
    unmount(instance!);

    render(null, 'GET /api/agents/izzie/subagents failed: 503');
    expect(target.textContent).toContain('Could not load sub-agents');
    expect(target.textContent).not.toContain('Resolving sub-agents…');
    unmount(instance!);

    render(payload());
    expect(target.textContent).not.toContain('Resolving sub-agents…');
    expect(target.textContent).not.toContain('Could not load sub-agents');
  });
});

describe('AgentConfigSubagents — the L0/L1 tier gate (#4169)', () => {
  // The single most important rendering rule: a target the gate refuses must
  // never read as callable. It is SHOWN (an operator needs to know why it is
  // missing) but under a refusal heading, with the gate's own reason.
  it('renders a tier-blocked target as refused, with its reason, not as reachable', () => {
    render(
      payload({
        in_product: inProduct({
          targets: [
            inTarget(),
            inTarget({
              name: 'orch',
              display_name: 'Orchestrator',
              role: 'assistant',
              tier: 'l0',
              reachable: false,
              reason: 'refused by the L0/L1 delegation gate (#4169)',
            }),
          ],
        }),
      }),
    );
    expect(target.textContent).toContain('Refused by the delegation gate (1)');
    expect(target.textContent).toContain('orch');
    expect(target.textContent).toContain('L0/L1');
    // The reachable card and the refused entry must not be confusable: exactly
    // ONE element carries the "reachable" badge, and it is not the L0 target.
    // (Asserted on badge ELEMENTS, not on `textContent` — the section's
    // explanatory prose legitimately uses the word too.)
    expect(reachableBadges()).toHaveLength(1);
  });

  it('shows no refusal block when every target is reachable', () => {
    render(payload());
    expect(target.textContent).not.toContain('Refused by the delegation gate');
  });

  it("reports this agent's own tier, the left-hand side of the gate", () => {
    render(payload({ in_product: inProduct({ delegator_tier: 'l0' }) }));
    expect(target.textContent).toContain('l0');
  });
});

describe('AgentConfigSubagents — registered vs granted', () => {
  it('says the tool is not registered for a non-assistant role', () => {
    render(payload({ in_product: inProduct({ tool_registered: false, tool_granted: false }) }));
    expect(target.textContent).toContain('not registered for this agent');
    expect(target.textContent).toContain('Nothing below is callable');
  });

  it('distinguishes registered-but-ungranted from not-registered', () => {
    render(payload({ in_product: inProduct({ tool_registered: true, tool_granted: false }) }));
    expect(target.textContent).toContain('registered for this role but');
    expect(target.textContent).toContain('[tools].allow');
    expect(target.textContent).not.toContain('not registered for this agent');
  });
});

describe('AgentConfigSubagents — cross-product deny-by-default (OQ-7)', () => {
  it('teaches that an absent [subagents] section grants nothing, not everything', () => {
    render(payload());
    expect(target.textContent).toContain('declares no');
    expect(target.textContent).toContain('does not mean "all"');
    expect(target.textContent).toContain('0 of 2 cross-product specialists granted');
  });

  it('renders every floor specialist with its grant state, denied ones included', () => {
    render(payload());
    expect(target.textContent).toContain('research');
    expect(target.textContent).toContain('ticketing');
    expect(target.textContent?.match(/denied/g)).toHaveLength(2);
  });

  it('grants only the declared floor target, leaving the other denied', () => {
    render(
      payload({
        cross_product: crossProduct({
          tool_granted: true,
          declares_allowed: true,
          targets: [
            { name: 'research', granted: true, reason: null },
            { name: 'ticketing', granted: false, reason: 'not listed in [subagents].allowed' },
          ],
        }),
      }),
    );
    expect(target.textContent).toContain('1 of 2 cross-product specialists granted');
    expect(target.textContent).not.toContain('does not mean "all"');
  });

  it('surfaces a declared name the bridge floor refuses instead of dropping it', () => {
    render(
      payload({
        cross_product: crossProduct({
          declares_allowed: true,
          rejected: [
            {
              name: 'engineer',
              reason: 'not a permitted non-coding specialist — the bridge floor hard-denies it',
            },
          ],
        }),
      }),
    );
    expect(target.textContent).toContain('Declared but refused by the bridge floor (1)');
    expect(target.textContent).toContain('engineer');
  });
});

describe('AgentConfigSubagents — honest degradation', () => {
  it('renders the fail-closed denial plus the parse error, never a blank pane', () => {
    render(
      payload({
        resolved: false,
        config_error: 'expected an equals, found an equals at line 1',
        in_product: inProduct({
          tool_registered: false,
          tool_granted: false,
          targets: [],
        }),
        cross_product: crossProduct(),
      }),
    );
    expect(target.textContent).toContain('could not be resolved');
    expect(target.textContent).toContain('fail-closed');
    expect(target.textContent).toContain('No agent with a delegatable role resolves');
    expect(target.textContent).toContain('0 of 2 cross-product specialists granted');
  });

  it('lists agents whose config did not resolve rather than dropping them', () => {
    render(
      payload({
        in_product: inProduct({
          unresolved: [{ name: 'halfbaked', reason: 'config did not resolve' }],
        }),
      }),
    );
    expect(target.textContent).toContain('1 agent whose config did not resolve');
    expect(target.textContent).toContain('halfbaked');
  });

  it('counts role-excluded roster entries without naming them', () => {
    render(payload({ in_product: inProduct({ role_excluded_count: 4 }) }));
    expect(target.textContent).toContain('4 further agent(s)');
    expect(target.textContent).toContain('excluded by the role');
  });
});
