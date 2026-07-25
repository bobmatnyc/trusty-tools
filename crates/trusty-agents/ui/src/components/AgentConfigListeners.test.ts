// Why (#3932, DOC-57 §6.2, L-1): the Listeners pane's defect was never that it
// was empty — it was that it stamped a hardcoded "not bound" badge on both
// connectors for every agent, asserting a per-agent binding state it had never
// read. That both invents a listener that may not exist and hides one that is
// quietly failing. This slice moves the section to its normative position and
// makes the pane say what it actually knows; #3937/#3891 replaces the constant
// with `GET /api/agents/:name/listeners` (C-05.1).
// What: Mounts the component (it takes no props — that is the point).
// Test: this file.
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { mount, unmount } from 'svelte';
import AgentConfigListeners from './AgentConfigListeners.svelte';

let target: HTMLDivElement;
let instance: Record<string, unknown> | null = null;

beforeEach(() => {
  target = document.createElement('div');
  document.body.appendChild(target);
  instance = mount(AgentConfigListeners, { target }) as unknown as Record<string, unknown>;
});

afterEach(() => {
  if (instance) {
    unmount(instance);
    instance = null;
  }
  target.remove();
});

describe('AgentConfigListeners', () => {
  it('labels itself scaffolding rather than this agent\'s configuration', () => {
    expect(target.textContent).toContain('Scaffolding, not configuration');
    expect(target.textContent).toContain('[[listeners]]');
  });

  it('claims no binding state for any connector (L-1)', () => {
    // The hardcoded per-listener badge is gone. The banner may (and does) use
    // the word "bound" to say the pane cannot see the bindings — asserting a
    // binding STATE is the defect, discussing the gap is the fix.
    expect(target.textContent).not.toContain('not bound');
  });

  it('still shows the connector definitions and their event kinds', () => {
    expect(target.textContent).toContain('Gmail');
    expect(target.textContent).toContain('Google Calendar');
    expect(target.textContent).toContain('message.received');
  });
});
