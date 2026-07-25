// Why (#3932, DOC-57 §7): Permissions is the section the spec creates rather
// than relabels, and its one hard rendering rule is PM-6 — every element
// carries an `enforced` marker, because displaying an unenforced control as if
// it constrained the agent is the permissions-surface equivalent of a
// fabricated listener pane. The second rule is polarity: an absent allow-list
// DENIES on the persona path (§7.1), so an empty pane that reads as
// "unrestricted" is actively misleading. The third is PM-4 — read-only, in
// every phase.
// What: Mounts the component directly with the two arrays `AgentDetail`
// already serves.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { mount, unmount } from 'svelte';
import AgentConfigPermissions from './AgentConfigPermissions.svelte';

let target: HTMLDivElement;
let instance: Record<string, unknown> | null = null;

function render(toolsAllow: string[], scopes: string[]) {
  instance = mount(AgentConfigPermissions, {
    target,
    props: { toolsAllow, scopes },
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

describe('AgentConfigPermissions', () => {
  it('renders the declared allow-list and scopes', () => {
    render(['gworkspace_*'], ['memory.read', 'google.gmail.*']);
    expect(target.textContent).toContain('gworkspace_*');
    expect(target.textContent).toContain('memory.read');
    expect(target.textContent).toContain('google.gmail.*');
  });

  it('marks every mechanism enforced or not enforced (PM-6)', () => {
    render(['gworkspace_*'], ['memory.read']);
    expect(target.textContent).toContain('enforced');
    // Scope enforcement is real but path-limited — the qualifier is not
    // decoration, it is the difference between the gate a user assumes and
    // the gate that runs.
    expect(target.textContent).toContain('path-limited');
    // `user_authority` has no field yet (#3074) and no value is invented.
    expect(target.textContent).toContain('not enforced');
    expect(target.textContent).toContain('reserved and does not exist');
  });

  it('states the deny polarity of an absent allow-list rather than reading as unrestricted', () => {
    render([], []);
    expect(target.textContent).toContain('No allow-list declared');
    expect(target.textContent).toContain('absent denies');
    expect(target.textContent).toContain('No scopes declared');
  });

  it('discloses that inherited values are not visible (PM-7)', () => {
    render(['gworkspace_*'], []);
    expect(target.textContent).toContain('extends');
  });

  it('offers no write path (PM-4)', () => {
    render(['gworkspace_*'], ['memory.read']);
    expect(Array.from(target.querySelectorAll('button, input, textarea, select'))).toHaveLength(0);
  });
});
