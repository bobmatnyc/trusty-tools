// Why (#3932, DOC-57 §5/§8.3): this pane replaced the Tools tab's single
// monospace textarea of raw globs, and G-3 is specific about the replacement —
// cards, with the wrapped tool list COLLAPSED, and a `synthetic` badge (S-10)
// whenever no manifest was authored. The badge is the measurable part: it is
// how an unwrapped surface stays visible instead of being indistinguishable
// from curated capability. The empty state matters just as much, because an
// absent allow-list denies rather than permits (§7.1) and a pane that renders
// blank there teaches the opposite.
// What: Mounts the component directly with `synthesizeSkills` output shapes.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { mount, unmount } from 'svelte';
import AgentConfigSkills from './AgentConfigSkills.svelte';
import { synthesizeSkills } from '../lib/agentConfig';

let target: HTMLDivElement;
let instance: Record<string, unknown> | null = null;

function render(toolsAllow: string[]) {
  instance = mount(AgentConfigSkills, {
    target,
    props: { skills: synthesizeSkills(toolsAllow) },
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

describe('AgentConfigSkills', () => {
  it('renders capability cards, not a glob textarea (G-3)', () => {
    render(['git_log', 'git_status', 'gworkspace_*']);
    expect(target.querySelector('textarea')).toBeNull();
    expect(target.textContent).toContain('git');
    expect(target.textContent).toContain('gworkspace');
  });

  it('badges every derived card synthetic and keeps its patterns collapsed (S-10, G-3)', () => {
    render(['git_log', 'git_status']);
    expect(target.textContent).toContain('synthetic');
    const details = target.querySelector('details') as HTMLDetailsElement;
    expect(details).not.toBeNull();
    expect(details.open).toBe(false);
    expect(details.textContent).toContain('git_log');
    expect(details.textContent).toContain('git_status');
  });

  it('states the deny polarity for an agent with no allow-list', () => {
    render([]);
    expect(target.textContent).toContain('declares no');
    expect(target.textContent).toContain('not "unrestricted"');
  });

  it('offers no write path in this phase (S-12) and points at the real edit surface', () => {
    render(['gworkspace_*']);
    expect(Array.from(target.querySelectorAll('button'))).toHaveLength(0);
    expect(target.textContent).toContain('agent.toml');
  });
});
