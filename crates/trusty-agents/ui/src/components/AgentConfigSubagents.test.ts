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
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mount, unmount, tick } from 'svelte';
vi.mock('../lib/agentConfig', () => ({ patchAgent: vi.fn(), fetchAgentSubagents: vi.fn() }));
import { patchAgent, fetchAgentSubagents } from '../lib/agentConfig';
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
    name: 'research-agent',
    display_name: 'Research',
    role: 'researcher',
    eligible: true,
    selected: true,
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
    allowed_roles: [
      'engineer',
      'qa',
      'researcher',
      'ticketing',
      'documentation',
      'ops',
      'planner',
      'assistant',
    ],
    // ADR-0024 decision 4: the reachable-set whitelist gate applies to the
    // assistant kind, and the default fixture declares one — the fail-closed
    // case has its own test below.
    whitelist_enforced: true,
    declares_whitelist: true,
    reachable_floor: ['research-agent', 'ticketing-agent'],
    targets: [inTarget()],
    role_excluded_count: 0,
    hidden_excluded_count: 0,
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

async function settle() { await Promise.resolve(); await tick(); await Promise.resolve(); await tick(); }
function render(data: AgentSubagents | null, error = '') {
  instance = mount(AgentConfigSubagents, { target, props: { agentName: 'alice', data, error } }) as unknown as Record<string, unknown>;
}
beforeEach(() => { target = document.createElement('div'); document.body.appendChild(target); });
afterEach(async () => { if (instance) await unmount(instance); instance = null; target.remove(); vi.resetAllMocks(); });

it('offers eligible agents without dumping refused roster entries', () => {
  render(payload({ in_product: inProduct({ targets: [inTarget(), inTarget({ name: 'peer', display_name: 'Peer assistant', eligible: false, reachable: false, selected: false, reason: 'kind gate' })] }) }));
  expect(target.querySelectorAll('input[type="checkbox"]')).toHaveLength(1);
  expect(target.textContent).toContain('Research');
  expect(target.textContent).not.toContain('Peer assistant');
  expect(target.textContent).not.toContain('Refused by the delegation gate');
});
it('persists the whitelist selection through the existing patch and refreshes it', async () => {
  const unchecked = inTarget({ selected: false, reachable: false });
  render(payload({ in_product: inProduct({ selected: [], targets: [unchecked] }) }));
  vi.mocked(patchAgent).mockResolvedValue({} as never);
  vi.mocked(fetchAgentSubagents).mockResolvedValue(payload({ in_product: inProduct({ selected: ['research-agent'] }) }));
  (target.querySelector('input') as HTMLInputElement).click(); await settle();
  expect(patchAgent).toHaveBeenCalledWith('alice', { subagents_delegate_allowed: ['research-agent'] });
  expect((target.querySelector('input') as HTMLInputElement).checked).toBe(true);
  expect(target.textContent).toContain('Saved.');
});
it('preserves existing selected names outside the visible candidates', async () => {
  render(payload({ in_product: inProduct({ selected: ['ticketing-agent'], targets: [inTarget({ selected: false, reachable: false })] }) }));
  vi.mocked(patchAgent).mockResolvedValue({} as never);
  vi.mocked(fetchAgentSubagents).mockResolvedValue(payload());
  (target.querySelector('input') as HTMLInputElement).click(); await settle();
  expect(patchAgent).toHaveBeenCalledWith('alice', { subagents_delegate_allowed: ['ticketing-agent', 'research-agent'] });
});
it('retains the selection and shows a reload action when saving fails', async () => {
  render(payload()); vi.mocked(patchAgent).mockRejectedValue(new Error('Save failed'));
  (target.querySelector('input') as HTMLInputElement).click(); await settle();
  expect((target.querySelector('input') as HTMLInputElement).checked).toBe(false);
  expect(target.querySelector('[role="alert"]')?.textContent).toContain('Could not confirm');
  expect(target.textContent).toContain('Reload delegation settings');
  expect(target.textContent).not.toContain('Saved.');
});
it('does not imply a selected whitelist grants the delegation tool', () => {
  render(payload({ in_product: inProduct({ tool_granted: false }) }));
  expect(target.textContent).toContain('Delegation is not enabled');
  expect(target.querySelector('input')).not.toBeNull();
});
it('shows role-ineligible agents as unavailable without controls', () => {
  render(payload({ in_product: inProduct({ tool_registered: false }) }));
  expect(target.textContent).toContain("Delegation is not available for this agent's role");
  expect(target.querySelector('input')).toBeNull();
});
it('keeps cross-product and coding delegation separate from allowed agents', () => {
  render(payload());
  expect(target.textContent).toContain('Allowed agents');
  expect(target.textContent).toContain('Cross-product');
  expect(target.textContent).toContain('Coding');
});
it('distinguishes loading, failed fetch and an empty eligible list', async () => {
  render(null); expect(target.textContent).toContain('Resolving');
  await unmount(instance!); instance = null;
  render(null, 'offline'); expect(target.textContent).toContain('offline');
  await unmount(instance!); instance = null;
  render(payload({ in_product: inProduct({ targets: [], selected: [] }) }));
  expect(target.textContent).toContain('No eligible delegation agents');
  expect(target.textContent).toContain('No agents selected');
});
