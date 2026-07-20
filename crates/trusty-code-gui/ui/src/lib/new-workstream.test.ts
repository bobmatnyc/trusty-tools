// Why: `lib/new-workstream.ts` carries every piece of pure gating/body-
// construction/name-inference logic `StartWorkingForm.svelte` depends on
// for correctness — covering it here means those invariants are checked
// without mounting Svelte or touching `fetch`, mirroring the split
// `create-session.test.ts` (this file's predecessor) established.
// What: One `describe` block per exported function.
// Test: this file.
import { describe, expect, it } from 'vitest';
import {
  bindingLabel,
  buildCreateWorkstreamBody,
  buildRunTaskBody,
  canSubmitCreate,
  extractTaskSessionId,
  extractWorkstreamId,
  inferWorkstreamName,
  isTaskRunResponse,
  isWorkstreamCreateResponse,
  type ProjectSelection,
} from './new-workstream';

const GIT_PROJECT: ProjectSelection = {
  path: '/Users/bob/code/acme-api',
  displayPath: 'acme-api',
  isGitRepo: true,
};

const NON_GIT_PROJECT: ProjectSelection = {
  path: '/Users/bob/code/scratch',
  displayPath: 'scratch',
  isGitRepo: false,
};

describe('inferWorkstreamName', () => {
  const FIXED_DATE = new Date('2026-07-19T12:00:00Z');

  it('uses the project display path plus the date when a project is selected', () => {
    expect(inferWorkstreamName(GIT_PROJECT, FIXED_DATE)).toBe('acme-api — 2026-07-19');
  });

  it('uses "new chat" plus the date when projectless', () => {
    expect(inferWorkstreamName(null, FIXED_DATE)).toBe('new chat — 2026-07-19');
  });
});

describe('buildCreateWorkstreamBody', () => {
  it('trims and includes a non-empty name', () => {
    expect(buildCreateWorkstreamBody('  acme-api — 2026-07-19  ')).toEqual({
      name: 'acme-api — 2026-07-19',
    });
  });

  it('omits name entirely for an empty or whitespace-only string', () => {
    expect(buildCreateWorkstreamBody('')).toEqual({});
    expect(buildCreateWorkstreamBody('   ')).toEqual({});
  });
});

describe('buildRunTaskBody', () => {
  it('trims the task and omits project/workstream_id when neither is given', () => {
    expect(buildRunTaskBody('  fix the bug  ', null)).toEqual({ task_description: 'fix the bug' });
  });

  it('includes project.path when a project is selected', () => {
    expect(buildRunTaskBody('add a feature', GIT_PROJECT)).toEqual({
      task_description: 'add a feature',
      project: '/Users/bob/code/acme-api',
    });
  });

  it('includes a non-git project path identically to a git one (AC-2.6 carried over)', () => {
    expect(buildRunTaskBody('index this', NON_GIT_PROJECT)).toEqual({
      task_description: 'index this',
      project: '/Users/bob/code/scratch',
    });
  });

  it('includes workstream_id when passed', () => {
    expect(buildRunTaskBody('t', null, 'ws-123')).toEqual({
      task_description: 't',
      workstream_id: 'ws-123',
    });
  });

  it('includes both project and workstream_id together', () => {
    expect(buildRunTaskBody('t', GIT_PROJECT, 'ws-123')).toEqual({
      task_description: 't',
      project: '/Users/bob/code/acme-api',
      workstream_id: 'ws-123',
    });
  });

  it('omits workstream_id when undefined, null, or empty', () => {
    expect(buildRunTaskBody('t', null, undefined)).toEqual({ task_description: 't' });
    expect(buildRunTaskBody('t', null, null)).toEqual({ task_description: 't' });
    expect(buildRunTaskBody('t', null, '')).toEqual({ task_description: 't' });
  });

  it('issue #3446: includes session_id when passed, for chat continuation', () => {
    expect(buildRunTaskBody('follow-up', null, null, 'sess-abc')).toEqual({
      task_description: 'follow-up',
      session_id: 'sess-abc',
    });
  });

  it('issue #3446: session_id and workstream_id are mutually exclusive — session_id wins, workstream_id is never sent alongside it', () => {
    expect(buildRunTaskBody('follow-up', null, 'ws-123', 'sess-abc')).toEqual({
      task_description: 'follow-up',
      session_id: 'sess-abc',
    });
  });

  it("issue #3446: includes project alongside session_id (restating a reused session's own binding is valid)", () => {
    expect(buildRunTaskBody('follow-up', GIT_PROJECT, null, 'sess-abc')).toEqual({
      task_description: 'follow-up',
      project: '/Users/bob/code/acme-api',
      session_id: 'sess-abc',
    });
  });

  it('issue #3446: omits session_id when undefined, null, or empty', () => {
    expect(buildRunTaskBody('t', null, 'ws-1', undefined)).toEqual({
      task_description: 't',
      workstream_id: 'ws-1',
    });
    expect(buildRunTaskBody('t', null, 'ws-1', null)).toEqual({
      task_description: 't',
      workstream_id: 'ws-1',
    });
    expect(buildRunTaskBody('t', null, 'ws-1', '')).toEqual({
      task_description: 't',
      workstream_id: 'ws-1',
    });
  });

  it('omits agent_name when not passed (daemon default, unchanged pre-#3449 behavior)', () => {
    const body = buildRunTaskBody('t', GIT_PROJECT, 'ws-1');
    expect('agent_name' in body).toBe(false);
  });

  it('includes agent_name when explicitly selected (issue #3449), alongside workstream_id', () => {
    expect(buildRunTaskBody('t', GIT_PROJECT, 'ws-1', null, 'rust-engineer')).toEqual({
      task_description: 't',
      project: '/Users/bob/code/acme-api',
      workstream_id: 'ws-1',
      agent_name: 'rust-engineer',
    });
  });

  it("includes agent_name alongside session_id too (issue #3449 + #3446: a resumed session's per-run agent must not silently reset to the daemon default)", () => {
    expect(buildRunTaskBody('follow-up', null, null, 'sess-abc', 'rust-engineer')).toEqual({
      task_description: 'follow-up',
      session_id: 'sess-abc',
      agent_name: 'rust-engineer',
    });
  });

  it('omits agent_name when undefined, null, or empty (the "daemon default" selector option)', () => {
    expect(buildRunTaskBody('t', null, null, null, undefined)).toEqual({ task_description: 't' });
    expect(buildRunTaskBody('t', null, null, null, null)).toEqual({ task_description: 't' });
    expect(buildRunTaskBody('t', null, null, null, '')).toEqual({ task_description: 't' });
  });
});

describe('canSubmitCreate', () => {
  it('is false for an empty or whitespace-only task', () => {
    expect(canSubmitCreate('', 'idle')).toBe(false);
    expect(canSubmitCreate('   ', 'idle')).toBe(false);
  });

  it('is true for a non-empty task while idle', () => {
    expect(canSubmitCreate('do the thing', 'idle')).toBe(true);
  });

  it('is false while submitting, even with a valid task (no double submit)', () => {
    expect(canSubmitCreate('do the thing', 'submitting')).toBe(false);
  });
});

describe('bindingLabel', () => {
  it('labels a null selection as projectless', () => {
    expect(bindingLabel(null)).toBe('projectless — chat/planning only');
  });

  it('labels a git-repo selection distinctly from a plain directory', () => {
    expect(bindingLabel(GIT_PROJECT)).toBe('git repo — acme-api');
    expect(bindingLabel(NON_GIT_PROJECT)).toBe('directory — scratch');
  });
});

describe('isWorkstreamCreateResponse', () => {
  it('accepts a well-formed POST /workstreams 201 body', () => {
    expect(isWorkstreamCreateResponse({ id: 'ws-123' })).toBe(true);
  });

  it('rejects non-object bodies and bodies missing/mistyping id', () => {
    expect(isWorkstreamCreateResponse(null)).toBe(false);
    expect(isWorkstreamCreateResponse('ws-123')).toBe(false);
    expect(isWorkstreamCreateResponse({})).toBe(false);
    expect(isWorkstreamCreateResponse({ id: 42 })).toBe(false);
  });
});

describe('extractWorkstreamId', () => {
  it('returns the id when it is a non-empty string', () => {
    expect(extractWorkstreamId({ id: 'ws-123' })).toBe('ws-123');
  });

  it('returns null for missing, empty, or non-string ids and non-object bodies', () => {
    expect(extractWorkstreamId({})).toBeNull();
    expect(extractWorkstreamId({ id: '' })).toBeNull();
    expect(extractWorkstreamId({ id: 42 })).toBeNull();
    expect(extractWorkstreamId(null)).toBeNull();
  });
});

describe('isTaskRunResponse', () => {
  it('accepts a well-formed POST /tasks 202 body', () => {
    expect(
      isTaskRunResponse({
        session_id: 'sess-abc12345',
        status: 'running',
        mode: 'auto',
        binding: { state: 'projectless', root: null },
      }),
    ).toBe(true);
  });

  it('rejects non-object bodies and bodies missing/mistyping session_id', () => {
    expect(isTaskRunResponse(null)).toBe(false);
    expect(isTaskRunResponse('sess-abc')).toBe(false);
    expect(isTaskRunResponse({})).toBe(false);
    expect(isTaskRunResponse({ session_id: 42 })).toBe(false);
  });
});

describe('extractTaskSessionId', () => {
  it('returns the id when it is a non-empty string', () => {
    expect(extractTaskSessionId({ session_id: 'sess-abc12345', status: 'running' })).toBe(
      'sess-abc12345',
    );
  });

  it('returns null for missing, empty, or non-string ids and non-object bodies', () => {
    expect(extractTaskSessionId({})).toBeNull();
    expect(extractTaskSessionId({ session_id: '' })).toBeNull();
    expect(extractTaskSessionId({ session_id: 42 })).toBeNull();
    expect(extractTaskSessionId(null)).toBeNull();
    expect(extractTaskSessionId('sess-abc')).toBeNull();
  });
});
