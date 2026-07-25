// Why (#3932): the Personality body moved out of `AgentConfigPanel` into its
// own component, and two of its properties are easy to lose in that move —
// the save button's disabled-until-dirty contract (the shell owns `dirty`, so
// the wiring is now a prop, not a local `$:`), and DOC-57 §3.2's P-2, which
// forbids the fabricated per-connector-instructions block this pane used to
// render (a `DEFINED_LISTENERS` loop asserting "no event instructions
// configured" for connectors it never looked at).
// What: Mounts the component directly with props — no fetch, no shell.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount } from 'svelte';
import AgentConfigPersonality from './AgentConfigPersonality.svelte';

let target: HTMLDivElement;
let instance: Record<string, unknown> | null = null;

function render(props: Record<string, unknown>) {
  instance = mount(AgentConfigPersonality, {
    target,
    props: { personaEditable: true, content: '', dirty: false, saving: false, justSaved: false, onSave: vi.fn(), ...props },
  }) as unknown as Record<string, unknown>;
}

const saveButton = () =>
  Array.from(target.querySelectorAll('button')).find((b) =>
    b.textContent?.includes('Save'),
  ) as HTMLButtonElement;

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

describe('AgentConfigPersonality', () => {
  it('offers no editor for a flat agent that carries no persona.md', () => {
    render({ personaEditable: false });
    expect(target.querySelector('[data-instructions-editor]')).toBeNull();
    expect(target.textContent).toContain('no editable persona.md');
  });

  it('disables Save until the shell reports the buffer dirty', () => {
    render({ content: 'You are Izzie.', dirty: false });
    expect(saveButton().disabled).toBe(true);
  });

  it('calls back to the shell to save a dirty buffer', () => {
    const onSave = vi.fn();
    render({ content: 'edited', dirty: true, onSave });
    saveButton().click();
    expect(onSave).toHaveBeenCalledTimes(1);
  });

  it('states the per-connector-instructions gap instead of enumerating connectors (P-2)', () => {
    render({ content: 'You are Izzie.' });
    // The block must be backed by the real `events/*.md` file set or say
    // nothing about it. No route lists those files, so naming connectors here
    // would assert a per-agent fact this pane never checked.
    expect(target.textContent).toContain('events/<connector>.md');
    expect(target.textContent).not.toContain('Gmail');
    expect(target.textContent).not.toContain('Google Calendar');
  });
});
