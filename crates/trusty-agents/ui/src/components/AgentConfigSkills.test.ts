// Why (#3933, DOC-57 §5/§8.3): this pane's predecessor grouped `[tools].allow`
// globs by their `_` prefix and badged every card `synthetic` — honest about
// being a guess, but it showed the user prefix fragments rather than
// capabilities. The owner's model is one skill per tool with a HUMAN name, so
// the properties worth pinning are: a card carries prose and its single tool
// (never a family), a credential is never claimed without evidence, an
// unresolved grant is surfaced rather than dropped, and the empty state teaches
// the deny-on-absent polarity (§7.1) instead of rendering blank.
// What: Mounts the component directly with `AgentSkills` wire shapes.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { mount, unmount } from 'svelte';
import AgentConfigSkills from './AgentConfigSkills.svelte';
import type { AgentSkill, AgentSkills } from '../lib/agentConfig';

let target: HTMLDivElement;
let instance: Record<string, unknown> | null = null;

function skill(over: Partial<AgentSkill> = {}): AgentSkill {
  return {
    id: 'mta-train-time',
    name: 'MTA Train Time',
    description: 'Look up the next Metro-North departures between two stations.',
    kind: 'action',
    origin: { kind: 'builtin' },
    granted: true,
    tools: ['get_train_schedule'],
    provider: null,
    // #4022/#4025: additive fields, inert on a leaf card.
    members: [],
    granted_members: [],
    granted_state: 'all',
    ...over,
  };
}

function payload(over: Partial<AgentSkills> = {}): AgentSkills {
  return {
    skills: [skill()],
    granted_count: 1,
    groups: [],
    unresolved: [],
    unmatched_patterns: [],
    declares_capability: true,
    ...over,
  };
}

function render(data: AgentSkills | null, error = '') {
  instance = mount(AgentConfigSkills, {
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

describe('AgentConfigSkills', () => {
  it('renders the human name and the single wrapped tool, not a glob textarea', () => {
    render(payload());
    expect(target.querySelector('textarea')).toBeNull();
    expect(target.textContent).toContain('MTA Train Time');
    expect(target.textContent).toContain('get_train_schedule');
    expect(target.textContent).toContain('Metro-North departures');
  });

  it('lists only granted skills and reports the rest as a count', () => {
    render(
      payload({
        skills: [skill(), skill({ id: 'gmail-search', name: 'Gmail Search', granted: false })],
        granted_count: 1,
      }),
    );
    expect(target.textContent).toContain('MTA Train Time');
    expect(target.textContent).not.toContain('Gmail Search');
    expect(target.textContent).toContain('1 of 2 known skills granted');
  });

  it('groups by kind so knowledge and system capability are separable', () => {
    render(
      payload({
        skills: [
          skill(),
          skill({ id: 'knowledge-search', name: 'Knowledge Search', kind: 'knowledge' }),
          skill({ id: 'task-finish', name: 'Finish the Task', kind: 'system' }),
        ],
      }),
    );
    expect(target.textContent).toContain('Actions (1)');
    expect(target.textContent).toContain('Knowledge (1)');
    expect(target.textContent).toContain('System (1)');
  });

  it('renders a tool-less skill as guidance rather than as a broken card', () => {
    render(
      payload({
        skills: [
          skill({ id: 'handoff-protocol', name: 'Work Handoff', kind: 'system', tools: [] }),
        ],
      }),
    );
    expect(target.textContent).toContain('Work Handoff');
    expect(target.textContent).toContain('guidance only — no tool');
  });

  it('never claims an unverified credential is configured', () => {
    render(
      payload({
        skills: [
          skill({
            id: 'gmail-search',
            name: 'Gmail Search',
            provider: {
              provider: 'Google Workspace',
              requirement: 'An authorized Google account profile.',
              env_var: null,
              configured: null,
            },
          }),
        ],
      }),
    );
    expect(target.textContent).toContain('Google Workspace — not verified');
    expect(target.textContent).not.toContain('Google Workspace configured');
  });

  it('says NOT configured when an env-backed credential is genuinely absent', () => {
    render(
      payload({
        skills: [
          skill({
            provider: {
              provider: 'MTA',
              requirement: 'An MTA developer API key.',
              env_var: 'MTA_API_KEY',
              configured: false,
            },
          }),
        ],
      }),
    );
    expect(target.textContent).toContain('MTA NOT configured');
  });

  it('badges a derived skill and admits it has no authored description', () => {
    render(
      payload({
        skills: [
          skill({
            id: 'derived:granola_list_meetings',
            name: 'Granola List Meetings',
            description: '',
            origin: { kind: 'derived' },
            tools: ['granola_list_meetings'],
          }),
        ],
      }),
    );
    expect(target.textContent).toContain('derived');
    expect(target.textContent).toContain('No description authored');
  });

  it('surfaces an unresolved skill grant instead of dropping it (S-11)', () => {
    render(
      payload({
        unresolved: [{ id: 'typo-skill', reason: 'no skill with this id' }],
      }),
    );
    expect(target.textContent).toContain('Unresolved skill grants (1)');
    expect(target.textContent).toContain('typo-skill');
  });

  it('reports an unmatched allow-pattern honestly, collapsed', () => {
    render(
      payload({
        unmatched_patterns: [
          { pattern: 'granola_*', reason: 'may resolve to an MCP tool at dispatch time' },
        ],
      }),
    );
    const details = target.querySelector('details') as HTMLDetailsElement;
    expect(details).not.toBeNull();
    expect(details.open).toBe(false);
    expect(details.textContent).toContain('granola_*');
  });

  it('states the deny polarity for an agent that declares no capability', () => {
    render(payload({ skills: [], granted_count: 0, declares_capability: false }));
    expect(target.textContent).toContain('declares neither');
    expect(target.textContent).toContain('not "unrestricted"');
  });

  it('states a config error rather than rendering an empty pane', () => {
    render(payload({ skills: [], config_error: 'expected an equals' }));
    expect(target.textContent).toContain('could not be parsed');
    expect(target.textContent).toContain('expected an equals');
  });

  it('reports a fetch failure instead of showing zero skills as fact', () => {
    render(null, 'Error: 503');
    expect(target.textContent).toContain('Could not load skills');
    expect(target.textContent).toContain('503');
  });

  it('offers no write path in this phase (S-12) and points at the real edit surface', () => {
    render(payload());
    expect(Array.from(target.querySelectorAll('button'))).toHaveLength(0);
    expect(target.textContent).toContain('agent.toml');
    expect(target.textContent).toContain('[skills].allow');
  });
});
