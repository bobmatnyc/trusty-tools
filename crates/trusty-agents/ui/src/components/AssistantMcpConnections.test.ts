import { afterEach, expect, it, vi } from 'vitest';
import { mount, unmount, tick } from 'svelte';
vi.mock('../lib/assistantMcp', async () => {
  const actual = await vi.importActual<typeof import('../lib/assistantMcp')>('../lib/assistantMcp');
  return {
    ...actual,
    fetchAssistantMcp: vi.fn(),
    saveAssistantMcpDisabled: vi.fn(),
  };
});
import { fetchAssistantMcp, saveAssistantMcpDisabled } from '../lib/assistantMcp';
import AssistantMcpConnections from './AssistantMcpConnections.svelte';

const github = { name: 'github', enabled: true, transport: { type: 'stdio', command: 'github-mcp' } };

const fixture = {
  assistant: 'izzie',
  global: [github],
  overrides: { servers: [] as typeof github[], disabled: [] as string[] },
  resolved: [github],
  statuses: [{ name: 'github', tier: 'global' as const, enabled: true, usable: true, reason: null }],
  issues: [] as { tier: 'global' | 'assistant'; path: string; detail: string; remedy: string }[],
};

let component: ReturnType<typeof mount> | undefined;
async function settle() { await Promise.resolve(); await tick(); await Promise.resolve(); await tick(); }
const box = () => document.querySelector('input[type="checkbox"]') as HTMLInputElement;
afterEach(async () => { if (component) await unmount(component); component = undefined; document.body.innerHTML = ''; vi.resetAllMocks(); });

it('shows the shared connections and the effective set', async () => {
  vi.mocked(fetchAssistantMcp).mockResolvedValue(fixture);
  component = mount(AssistantMcpConnections, { target: document.body, props: { agentName: 'izzie' } }); await settle();
  expect(document.body.textContent).toContain('github');
  expect(document.body.textContent).toContain('servers.toml');
  expect(document.body.textContent).toContain('available');
  expect(document.body.textContent).toContain('shared');
  expect(box().checked).toBe(true);
});

it('switching a shared connection off sends it as this assistant disabled list', async () => {
  vi.mocked(fetchAssistantMcp).mockResolvedValue(fixture);
  vi.mocked(saveAssistantMcpDisabled).mockResolvedValue({
    ...fixture,
    overrides: { servers: [], disabled: ['github'] },
    resolved: [],
    statuses: [],
  });
  component = mount(AssistantMcpConnections, { target: document.body, props: { agentName: 'izzie' } }); await settle();
  box().click(); await settle();
  // Only the disable list — never the servers the GET returned. Those carry
  // `<redacted>` in place of every transport secret, so echoing them back
  // would write the marker over a real credential.
  expect(saveAssistantMcpDisabled).toHaveBeenCalledWith('izzie', ['github']);
  expect(document.body.textContent).toContain('connects to no MCP server');
  expect(box().checked).toBe(false);
});

it('renders a server that is configured but not usable, with its reason', async () => {
  vi.mocked(fetchAssistantMcp).mockResolvedValue({
    ...fixture,
    statuses: [{
      name: 'github',
      tier: 'global' as const,
      enabled: true,
      usable: false,
      reason: 'credential environment variable `GITHUB_TOKEN` is not set',
    }],
  });
  component = mount(AssistantMcpConnections, { target: document.body, props: { agentName: 'izzie' } }); await settle();
  expect(document.body.textContent).toContain('unavailable');
  expect(document.body.textContent).toContain('GITHUB_TOKEN');
});

it('names an assistant-level addition as its own tier', async () => {
  const extra = { name: 'izzie-only', enabled: true, transport: { type: 'stdio', command: 'izzie-bin' } };
  vi.mocked(fetchAssistantMcp).mockResolvedValue({
    ...fixture,
    overrides: { servers: [extra], disabled: [] },
    resolved: [github, extra],
    statuses: [
      { name: 'github', tier: 'global' as const, enabled: true, usable: true, reason: null },
      { name: 'izzie-only', tier: 'assistant' as const, enabled: true, usable: true, reason: null },
    ],
  });
  component = mount(AssistantMcpConnections, { target: document.body, props: { agentName: 'izzie' } }); await settle();
  expect(document.body.textContent).toContain('Added for this assistant');
  expect(document.body.textContent).toContain('set for this assistant');
});

it('shows a configuration issue with its remedy rather than an empty pane', async () => {
  vi.mocked(fetchAssistantMcp).mockResolvedValue({
    ...fixture,
    global: [],
    resolved: [],
    statuses: [],
    issues: [{
      tier: 'global' as const,
      path: '/home/u/.trusty-tools/mcp/servers.toml',
      detail: 'the shared MCP server file could not be read',
      remedy: 'Fix the TOML in that file.',
    }],
  });
  component = mount(AssistantMcpConnections, { target: document.body, props: { agentName: 'izzie' } }); await settle();
  expect(document.body.textContent).toContain('could not be read');
  expect(document.body.textContent).toContain('Fix the TOML in that file.');
});

it('renders a server whose transport secrets came back redacted', async () => {
  const secretive = {
    name: 'github',
    enabled: true,
    transport: {
      type: 'stdio',
      command: 'github-mcp',
      env: { API_KEY: '<redacted>' },
    },
  };
  vi.mocked(fetchAssistantMcp).mockResolvedValue({
    ...fixture,
    global: [secretive],
    resolved: [secretive],
  });
  component = mount(AssistantMcpConnections, { target: document.body, props: { agentName: 'izzie' } }); await settle();
  // The pane identifies a server by its command, never by its credentials, so
  // the redacted shape changes nothing it renders.
  expect(document.body.textContent).toContain('github-mcp');
  expect(document.body.textContent).not.toContain('<redacted>');
  expect(box().checked).toBe(true);
});

it('shows a failed read as unavailable rather than as an assistant with no connections', async () => {
  vi.mocked(fetchAssistantMcp).mockRejectedValue(new Error('Assistant not found'));
  component = mount(AssistantMcpConnections, { target: document.body, props: { agentName: 'ghost' } }); await settle();
  expect(document.body.textContent).toContain('Assistant not found');
  expect(document.body.textContent).not.toContain('Effective connections');
});
